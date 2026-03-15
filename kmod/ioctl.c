// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — control interface via ioctl on any agfs directory fd.
 *
 * The permission daemon opens .agfs/mnt (or any dir on the mount) and uses:
 *   ioctl(fd, AGFS_IOC_GET_REQUEST, &req)  — dequeue pending ask request
 *   ioctl(fd, AGFS_IOC_PUT_RESPONSE, &resp) — submit decision
 *   ioctl(fd, AGFS_IOC_RULE_ADD, &rule)   — add permission rule
 *   ioctl(fd, AGFS_IOC_RULE_REMOVE, &rule)
 *   ioctl(fd, AGFS_IOC_CACHE_INVAL)
 *
 * On close, any dispatched-but-unanswered requests get the default decision.
 */

#include "agfs.h"
#include <linux/file.h>

/* ── Lazy-allocate ctl private on first GET_REQUEST ─────────────────── */

static struct agfs_ctl_private *ensure_ctl(struct file *file)
{
	struct agfs_file_info *fi = AGFS_F(file);
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);

	if (fi->ctl)
		return fi->ctl;

	/* Only one daemon allowed at a time */
	if (atomic_cmpxchg(&sbi->has_daemon, 0, 1) != 0)
		return ERR_PTR(-EBUSY);

	fi->ctl = kzalloc(sizeof(*fi->ctl), GFP_KERNEL);
	if (!fi->ctl) {
		atomic_set(&sbi->has_daemon, 0);
		return ERR_PTR(-ENOMEM);
	}
	INIT_LIST_HEAD(&fi->ctl->dispatched);
	spin_lock_init(&fi->ctl->lock);
	return fi->ctl;
}

/* ── GET_REQUEST: dequeue pending request ──────────────────────────── */

static long agfs_get_request_ioctl(struct file *file, unsigned long arg)
{
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);
	struct agfs_ctl_private *priv;
	struct agfs_perm_request *req;
	struct agfs_ctl_request out;
	int err;

	priv = ensure_ctl(file);
	if (IS_ERR(priv))
		return PTR_ERR(priv);

	/* Wait for a pending request */
	if (file->f_flags & O_NONBLOCK) {
		spin_lock(&sbi->pending_lock);
		if (list_empty(&sbi->pending_reqs)) {
			spin_unlock(&sbi->pending_lock);
			return -EAGAIN;
		}
	} else {
		err = wait_event_interruptible(sbi->request_waitq,
			!list_empty(&sbi->pending_reqs));
		if (err)
			return err;
		spin_lock(&sbi->pending_lock);
		if (list_empty(&sbi->pending_reqs)) {
			spin_unlock(&sbi->pending_lock);
			return -EAGAIN;
		}
	}

	req = list_first_entry(&sbi->pending_reqs,
			       struct agfs_perm_request, list);
	list_del_init(&req->list);
	kref_get(&req->ref); /* daemon takes a reference */
	spin_unlock(&sbi->pending_lock);

	memset(&out, 0, sizeof(out));
	out.id = req->id;
	out.op = req->op;
	out.pid = req->pid;
	strscpy(out.comm, req->comm, sizeof(out.comm));
	strscpy(out.path, req->path, sizeof(out.path));

	spin_lock(&priv->lock);
	list_add_tail(&req->list, &priv->dispatched);
	spin_unlock(&priv->lock);

	if (copy_to_user((void __user *)arg, &out, sizeof(out))) {
		spin_lock(&priv->lock);
		list_del_init(&req->list);
		spin_unlock(&priv->lock);

		spin_lock(&sbi->pending_lock);
		list_add(&req->list, &sbi->pending_reqs);
		spin_unlock(&sbi->pending_lock);
		wake_up_interruptible(&sbi->request_waitq);
		kref_put(&req->ref, agfs_perm_request_release);
		return -EFAULT;
	}

	return 0;
}

/* ── PUT_RESPONSE: submit decision ─────────────────────────────────── */

static long agfs_put_response_ioctl(struct file *file, unsigned long arg)
{
	struct agfs_file_info *fi = AGFS_F(file);
	struct agfs_ctl_private *priv = fi->ctl;
	struct agfs_ctl_response in;
	struct agfs_perm_request *req, *tmp;
	bool found = false;

	if (!priv)
		return -EINVAL;

	if (copy_from_user(&in, (void __user *)arg, sizeof(in)))
		return -EFAULT;

	if (in.decision > AGFS_PERM_DENY)
		return -EINVAL;

	spin_lock(&priv->lock);
	list_for_each_entry_safe(req, tmp, &priv->dispatched, list) {
		if (req->id == in.id) {
			req->decision = (enum agfs_perm)in.decision;
			list_del_init(&req->list);
			found = true;
			break;
		}
	}
	spin_unlock(&priv->lock);

	if (!found)
		return -ENOENT;

	complete(&req->done);
	kref_put(&req->ref, agfs_perm_request_release);
	return 0;
}

/* ── Cleanup dispatched requests on fd close ───────────────────────── */

void agfs_ctl_cleanup(struct agfs_sb_info *sbi, struct agfs_ctl_private *priv)
{
	struct agfs_perm_request *req, *tmp;

	spin_lock(&priv->lock);
	list_for_each_entry_safe(req, tmp, &priv->dispatched, list) {
		req->decision = sbi->ask_default;
		list_del_init(&req->list);
		complete(&req->done);
		kref_put(&req->ref, agfs_perm_request_release);
	}
	spin_unlock(&priv->lock);
	kfree(priv);
	atomic_set(&sbi->has_daemon, 0);
}

/* ── Rule ioctl helpers ─────────────────────────────────────────────── */

static int agfs_resolve_rule(struct file *file, unsigned long arg,
			     struct agfs_ioc_rule *rule,
			     struct path *rule_path,
			     struct agfs_dentry_info **di_out)
{
	int err;

	if (copy_from_user(rule, (void __user *)arg, sizeof(*rule)))
		return -EFAULT;

	rule->path[AGFS_PATH_MAX - 1] = '\0';

	err = kern_path(rule->path, LOOKUP_FOLLOW, rule_path);
	if (err)
		return err;

	if (rule_path->dentry->d_sb != file_inode(file)->i_sb) {
		path_put(rule_path);
		return -EXDEV;
	}

	*di_out = AGFS_D(rule_path->dentry);
	if (!*di_out) {
		path_put(rule_path);
		return -ENOENT;
	}

	return 0;
}

/* ── Unified ioctl handler (rules + ctl) ───────────────────────────── */

long agfs_ioctl(struct file *file, unsigned int cmd, unsigned long arg)
{
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);

	switch (cmd) {
	case AGFS_IOC_GET_REQUEST:
		return agfs_get_request_ioctl(file, arg);

	case AGFS_IOC_PUT_RESPONSE:
		return agfs_put_response_ioctl(file, arg);

	case AGFS_IOC_RULE_ADD: {
		struct agfs_ioc_rule rule;
		struct path rule_path;
		struct agfs_dentry_info *di;
		int err;

		err = agfs_resolve_rule(file, arg, &rule, &rule_path, &di);
		if (err)
			return err;

		if (rule.perm > AGFS_PERM_DENY) {
			path_put(&rule_path);
			return -EINVAL;
		}

		spin_lock(&di->lock);
		if (di->perm == AGFS_PERM_NONE)
			dget(rule_path.dentry);
		di->perm = (enum agfs_perm)rule.perm;
		spin_unlock(&di->lock);
		atomic64_inc(&sbi->perm_gen);
		path_put(&rule_path);
		return 0;
	}

	case AGFS_IOC_RULE_REMOVE: {
		struct agfs_ioc_rule rule;
		struct path rule_path;
		struct agfs_dentry_info *di;
		int err;

		err = agfs_resolve_rule(file, arg, &rule, &rule_path, &di);
		if (err)
			return err;

		spin_lock(&di->lock);
		if (di->perm != AGFS_PERM_NONE) {
			di->perm = AGFS_PERM_NONE;
			spin_unlock(&di->lock);
			dput(rule_path.dentry);
		} else {
			spin_unlock(&di->lock);
		}
		atomic64_inc(&sbi->perm_gen);
		path_put(&rule_path);
		return 0;
	}

	case AGFS_IOC_CACHE_INVAL:
		atomic64_inc(&sbi->perm_gen);
		shrink_dcache_sb(file_inode(file)->i_sb);
		/* Reopen journal — CLI deletes it on commit/abort */
		if (sbi->journal_file) {
			fput(sbi->journal_file);
			sbi->journal_file = NULL;
		}
		agfs_journal_open(sbi);
		return 0;

	case AGFS_IOC_SNAPSHOT: {
		struct agfs_ioc_snapshot snap;
		u64 gen;

		if (sbi->nostaging)
			return -EOPNOTSUPP;

		if (copy_from_user(&snap, (void __user *)arg, sizeof(snap)))
			return -EFAULT;

		snap.name[AGFS_PATH_MAX - 1] = '\0';

		down_write(&sbi->staging_sem);
		gen = atomic64_inc_return(&sbi->snapshot_gen);
		agfs_journal_append_s(sbi, gen, snap.name);
		up_write(&sbi->staging_sem);

		/* Best-effort: snapshot is already committed to the journal,
		 * so return success even if copy_to_user fails. */
		snap.id = gen;
		if (copy_to_user((void __user *)arg, &snap, sizeof(snap)))
			/* id already in journal — userspace can read it back */;

		return 0;
	}

	default:
		return -ENOTTY;
	}
}
