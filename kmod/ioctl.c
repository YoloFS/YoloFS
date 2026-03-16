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

/* ── Claim daemon connection on first GET_REQUEST ──────────────────── */

static int agfs_daemon_connect(struct file *file)
{
	struct agfs_ask_engine *eng = &AGFS_SB(file_inode(file)->i_sb)->ask_engine;

	spin_lock(&eng->dispatch_lock);
	if (eng->daemon_file) {
		spin_unlock(&eng->dispatch_lock);
		return -EBUSY;
	}
	eng->daemon_file = file;
	spin_unlock(&eng->dispatch_lock);
	return 0;
}

/* ── GET_REQUEST: dequeue pending request ──────────────────────────── */

static long agfs_get_request_ioctl(struct file *file, unsigned long arg)
{
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);
	struct agfs_ask_engine *eng = &sbi->ask_engine;
	struct agfs_perm_request *req;
	struct agfs_ctl_request out;
	int err;

	if (READ_ONCE(eng->daemon_file) != file) {
		err = agfs_daemon_connect(file);
		if (err)
			return err;
	}

	/* Wait for a pending request */
	if (file->f_flags & O_NONBLOCK) {
		spin_lock(&eng->pending_lock);
		if (list_empty(&eng->pending_reqs)) {
			spin_unlock(&eng->pending_lock);
			return -EAGAIN;
		}
	} else {
		err = wait_event_interruptible(eng->request_waitq,
			!list_empty(&eng->pending_reqs));
		if (err)
			return err;
		spin_lock(&eng->pending_lock);
		if (list_empty(&eng->pending_reqs)) {
			spin_unlock(&eng->pending_lock);
			return -EAGAIN;
		}
	}

	req = list_first_entry(&eng->pending_reqs,
			       struct agfs_perm_request, list);
	list_del_init(&req->list);
	kref_get(&req->ref); /* daemon takes a reference */
	spin_unlock(&eng->pending_lock);

	memset(&out, 0, sizeof(out));
	out.id = req->id;
	out.op = req->op;
	out.pid = req->pid;
	strscpy(out.comm, req->comm, sizeof(out.comm));
	strscpy(out.path, req->path, sizeof(out.path));

	spin_lock(&eng->dispatch_lock);
	list_add_tail(&req->list, &eng->dispatched);
	spin_unlock(&eng->dispatch_lock);

	if (copy_to_user((void __user *)arg, &out, sizeof(out))) {
		spin_lock(&eng->dispatch_lock);
		list_del_init(&req->list);
		spin_unlock(&eng->dispatch_lock);

		spin_lock(&eng->pending_lock);
		list_add(&req->list, &eng->pending_reqs);
		spin_unlock(&eng->pending_lock);
		wake_up_interruptible(&eng->request_waitq);
		kref_put(&req->ref, agfs_perm_request_release);
		return -EFAULT;
	}

	return 0;
}

/* ── PUT_RESPONSE: submit decision ─────────────────────────────────── */

static long agfs_put_response_ioctl(struct file *file, unsigned long arg)
{
	struct agfs_ask_engine *eng = &AGFS_SB(file_inode(file)->i_sb)->ask_engine;
	struct agfs_ctl_response in;
	struct agfs_perm_request *req, *tmp;
	bool found = false;

	if (READ_ONCE(eng->daemon_file) != file)
		return -EINVAL;

	if (copy_from_user(&in, (void __user *)arg, sizeof(in)))
		return -EFAULT;

	if (in.decision > AGFS_PERM_DENY)
		return -EINVAL;

	spin_lock(&eng->dispatch_lock);
	list_for_each_entry_safe(req, tmp, &eng->dispatched, list) {
		if (req->id == in.id) {
			req->decision = (enum agfs_perm)in.decision;
			list_del_init(&req->list);
			found = true;
			break;
		}
	}
	spin_unlock(&eng->dispatch_lock);

	if (!found)
		return -ENOENT;

	complete(&req->done);
	kref_put(&req->ref, agfs_perm_request_release);
	return 0;
}

/* ── Cleanup dispatched requests on daemon fd close ────────────────── */

void agfs_daemon_cleanup(struct agfs_sb_info *sbi)
{
	struct agfs_ask_engine *eng = &sbi->ask_engine;
	struct agfs_perm_request *req, *tmp;

	spin_lock(&eng->dispatch_lock);
	list_for_each_entry_safe(req, tmp, &eng->dispatched, list) {
		req->decision = eng->default_perm;
		list_del_init(&req->list);
		complete(&req->done);
		kref_put(&req->ref, agfs_perm_request_release);
	}
	WRITE_ONCE(eng->daemon_file, NULL);
	spin_unlock(&eng->dispatch_lock);
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
		agfs_release_pinned_dirs(sbi);
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

		if (!sbi->staging)
			return -EOPNOTSUPP;

		if (copy_from_user(&snap, (void __user *)arg, sizeof(snap)))
			return -EFAULT;

		snap.name[AGFS_PATH_MAX - 1] = '\0';

		down_write(&sbi->staging_sem);
		if (atomic_read(&sbi->staging_fd_count) > 0) {
			up_write(&sbi->staging_sem);
			return -EBUSY;
		}
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
