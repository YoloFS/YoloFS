// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — control interface via ioctl on any agfs directory fd.
 *
 * The permission daemon opens .agfs/mnt (or any dir on the mount) and uses:
 *   ioctl(fd, AGFS_IOC_GET_REQUEST, &req)  — dequeue pending ask request
 *   ioctl(fd, AGFS_IOC_PUT_RESPONSE, &resp) — submit decision
 *   ioctl(fd, AGFS_IOC_RULE_ADD, &rule)   — add permission rule
 *   ioctl(fd, AGFS_IOC_RULE_REMOVE, &rule)
 *   ioctl(fd, AGFS_IOC_RESTORE)     — reset staging / restore to checkpoint
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
	__u16 path_len;

	if (READ_ONCE(eng->daemon_file) != file) {
		err = agfs_daemon_connect(file);
		if (err)
			return err;
	}

	/* Read buffer info from userspace */
	if (copy_from_user(&out, (void __user *)arg, sizeof(out)))
		return -EFAULT;

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

	path_len = strlen(req->path);

	if (path_len > out.path_buf_len) {
		err = -EOVERFLOW;
		goto requeue_pending;
	}

	out.id = req->id;
	out.op = req->op;
	out.pid = req->pid;
	strscpy(out.comm, req->comm, sizeof(out.comm));
	out.path_len = path_len;

	spin_lock(&eng->dispatch_lock);
	list_add_tail(&req->list, &eng->dispatched);
	spin_unlock(&eng->dispatch_lock);

	/* Write path data to user buffer */
	if (copy_to_user((void __user *)out.path_ptr, req->path, path_len)) {
		err = -EFAULT;
		goto requeue_dispatched;
	}

	/* Write header back to userspace */
	if (copy_to_user((void __user *)arg, &out, sizeof(out))) {
		err = -EFAULT;
		goto requeue_dispatched;
	}

	return 0;

requeue_dispatched:
	spin_lock(&eng->dispatch_lock);
	list_del_init(&req->list);
	spin_unlock(&eng->dispatch_lock);
requeue_pending:
	spin_lock(&eng->pending_lock);
	list_add(&req->list, &eng->pending_reqs);
	spin_unlock(&eng->pending_lock);
	wake_up_interruptible(&eng->request_waitq);
	kref_put(&req->ref, agfs_perm_request_release);
	return err;
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

/* ── Release all rule-pinned dentries ───────────────────────────────── */

void agfs_release_pinned_rules(struct agfs_sb_info *sbi)
{
	LIST_HEAD(local);
	struct agfs_dentry_info *di, *tmp;

	spin_lock(&sbi->pinned_rules_lock);
	list_splice_init(&sbi->pinned_rules, &local);
	spin_unlock(&sbi->pinned_rules_lock);

	list_for_each_entry_safe(di, tmp, &local, rule_pin) {
		struct dentry *dentry = di->rule_dentry;

		list_del_init(&di->rule_pin);
		spin_lock(&di->lock);
		di->perm = AGFS_PERM_NONE;
		di->rule_dentry = NULL;
		spin_unlock(&di->lock);
		dput(dentry);
	}
}

/* ── Path copy helper ──────────────────────────────────────────────── */

/*
 * Copy a variable-length path from userspace into a caller-provided buffer.
 * The buffer must be at least AGFS_PATH_MAX bytes.
 * Paths are limited to AGFS_PATH_MAX-1 bytes (same as internal buffers).
 */
static int agfs_copy_user_path(__u64 ptr, __u16 len, char *buf)
{
	if (!ptr || len == 0 || len >= AGFS_PATH_MAX)
		return -EINVAL;

	if (copy_from_user(buf, (const void __user *)ptr, len))
		return -EFAULT;
	buf[len] = '\0';

	return 0;
}

static int agfs_resolve_rule(struct file *file, unsigned long arg,
			     struct agfs_ioc_rule *rule,
			     struct path *rule_path,
			     struct agfs_dentry_info **di_out)
{
	char path_buf[AGFS_PATH_MAX];
	int err;

	if (copy_from_user(rule, (void __user *)arg, sizeof(*rule)))
		return -EFAULT;

	err = agfs_copy_user_path(rule->path_ptr, rule->path_len, path_buf);
	if (err)
		return err;

	err = kern_path(path_buf, LOOKUP_FOLLOW, rule_path);
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

/* ── Rule / checkpoint ioctl handlers ─────────────────────────────────── */

static long agfs_rule_add_ioctl(struct file *file, unsigned long arg)
{
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);
	struct agfs_ioc_rule rule;
	struct path rule_path;
	struct agfs_dentry_info *di;
	bool first;
	int err;

	err = agfs_resolve_rule(file, arg, &rule, &rule_path, &di);
	if (err)
		return err;

	if (rule.perm > AGFS_PERM_DENY) {
		path_put(&rule_path);
		return -EINVAL;
	}

	spin_lock(&di->lock);
	first = (di->perm == AGFS_PERM_NONE);
	if (first) {
		dget(rule_path.dentry);
		di->rule_dentry = rule_path.dentry;
	}
	di->perm = (enum agfs_perm)rule.perm;
	spin_unlock(&di->lock);

	if (first) {
		spin_lock(&sbi->pinned_rules_lock);
		if (list_empty(&di->rule_pin))
			list_add(&di->rule_pin, &sbi->pinned_rules);
		spin_unlock(&sbi->pinned_rules_lock);
	}

	atomic64_inc(&sbi->perm_gen);
	path_put(&rule_path);
	return 0;
}

static long agfs_rule_remove_ioctl(struct file *file, unsigned long arg)
{
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);
	struct agfs_ioc_rule rule;
	struct path rule_path;
	struct agfs_dentry_info *di;
	bool had_rule;
	int err;

	err = agfs_resolve_rule(file, arg, &rule, &rule_path, &di);
	if (err)
		return err;

	spin_lock(&di->lock);
	had_rule = (di->perm != AGFS_PERM_NONE);
	if (had_rule) {
		di->perm = AGFS_PERM_NONE;
		di->rule_dentry = NULL;
	}
	spin_unlock(&di->lock);

	if (had_rule) {
		spin_lock(&sbi->pinned_rules_lock);
		if (!list_empty(&di->rule_pin))
			list_del_init(&di->rule_pin);
		spin_unlock(&sbi->pinned_rules_lock);
		dput(rule_path.dentry);
	}

	atomic64_inc(&sbi->perm_gen);
	path_put(&rule_path);
	return 0;
}

static long agfs_checkpoint_ioctl(struct file *file, unsigned long arg)
{
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);
	struct agfs_ioc_checkpoint chk;
	char name_buf[AGFS_PATH_MAX];
	u64 gen;
	int err;

	if (!sbi->staging)
		return -EOPNOTSUPP;

	if (copy_from_user(&chk, (void __user *)arg, sizeof(chk)))
		return -EFAULT;

	err = agfs_copy_user_path(chk.name_ptr, chk.name_len, name_buf);
	if (err)
		return err;

	down_write(&sbi->staging_sem);
	if (atomic_read(&sbi->staging_fd_count) > 0) {
		up_write(&sbi->staging_sem);
		return -EBUSY;
	}
	gen = atomic64_inc_return(&sbi->gen);
	agfs_journal_checkpoint(sbi, gen, name_buf);
	up_write(&sbi->staging_sem);

	/* Best-effort: checkpoint is already committed to the journal,
	 * so return success even if copy_to_user fails. */
	chk.gen = gen;
	if (copy_to_user((void __user *)arg, &chk, sizeof(chk)))
		/* gen already in journal — userspace can read it back */;

	return 0;
}

/* ── Restore ioctl handler ─────────────────────────────────────────── */

/*
 * Split an absolute path into parent directory and child name.
 * Returns pointer to the child name within path. Sets *parent_len
 * to the byte length of the parent portion (excluding NUL).
 * E.g. "/src/main.rs" → parent_len=4 ("/src"), child="main.rs".
 *       "/README" → parent_len=1 ("/"), child="README".
 */
static const char *split_parent_child(const char *path, int *parent_len)
{
	const char *last_slash;

	last_slash = strrchr(path, '/');
	if (!last_slash || last_slash == path) {
		*parent_len = 1; /* "/" */
		return last_slash ? last_slash + 1 : path;
	}
	*parent_len = last_slash - path;
	return last_slash + 1;
}

static int agfs_restore_inject(struct file *file, struct agfs_sb_info *sbi,
			       struct agfs_ioc_restore *hdr, u64 gen)
{
	struct super_block *sb = file_inode(file)->i_sb;
	struct agfs_ioc_restore_entry __user *uentries =
		(struct agfs_ioc_restore_entry __user *)hdr->entries_ptr;
	struct agfs_ioc_restore_entry ent;
	u64 i;
	int err = 0;

	/*
	 * Inject dirent entries.  On failure midway, the mount is left
	 * with a partial set of dirents — the CLI can retry or abort.
	 */
	for (i = 0; i < hdr->entry_count; i++) {
		char path_buf[AGFS_PATH_MAX];
		char bp_buf[AGFS_PATH_MAX];
		struct agfs_dirent de;
		const char *child;
		int parent_len;
		char saved;
		struct path parent_path;
		struct inode *dir;
		char *bp;

		if (copy_from_user(&ent, &uentries[i], sizeof(ent))) {
			err = -EFAULT;
			break;
		}

		err = agfs_copy_user_path(ent.path_ptr, ent.path_len,
					  path_buf);
		if (err)
			break;

		bp = NULL;
		if (ent.base_len > 0) {
			err = agfs_copy_user_path(ent.base_ptr,
						  ent.base_len, bp_buf);
			if (err)
				break;
			bp = bp_buf;
		}

		child = split_parent_child(path_buf, &parent_len);
		if (!*child) {
			err = -EINVAL;
			break;
		}

		if (parent_len >= AGFS_PATH_MAX) {
			err = -ENAMETOOLONG;
			break;
		}

		/* NUL-terminate path_buf at the parent boundary in-place */
		saved = path_buf[parent_len];
		path_buf[parent_len] = '\0';

		err = vfs_path_lookup(sb->s_root, file->f_path.mnt,
				      path_buf,
				      LOOKUP_FOLLOW | LOOKUP_DIRECTORY,
				      &parent_path);

		path_buf[parent_len] = saved;

		if (err)
			break;

		dir = d_inode(parent_path.dentry);

		de = (struct agfs_dirent){
			.ino = ent.ino,
			.base = bp,
			.overwrites = ent.overwrites,
			.d_type = ent.d_type,
			.gen = gen,
		};
		inode_lock(dir);
		err = agfs_add_dirent(dir, child, strlen(child), &de);
		inode_unlock(dir);
		path_put(&parent_path);

		if (err)
			break;
	}

	return err;
}

static long agfs_restore_ioctl(struct file *file, unsigned long arg)
{
	struct super_block *sb = file_inode(file)->i_sb;
	struct agfs_sb_info *sbi = AGFS_SB(sb);
	struct agfs_ioc_restore hdr;
	u64 new_gen;
	int err = 0;

	if (!sbi->staging)
		return -EOPNOTSUPP;

	if (copy_from_user(&hdr, (void __user *)arg, sizeof(hdr)))
		return -EFAULT;

	down_write(&sbi->staging_sem);
	if (atomic_read(&sbi->staging_fd_count) > 0) {
		up_write(&sbi->staging_sem);
		return -EBUSY;
	}

	/* Wipe perm caches, dirents, dentry cache */
	atomic64_inc(&sbi->perm_gen);
	agfs_release_pinned_dirs(sbi);
	shrink_dcache_sb(sb);

	if (hdr.target_gen == 0) {
		/* Reset mode (commit/abort): no entries, no journal write */
		atomic64_set(&sbi->gen, 1);
		up_write(&sbi->staging_sem);
		return 0;
	}

	/* Restore mode: increment gen, inject entries, write S record */
	new_gen = atomic64_inc_return(&sbi->gen);

	err = agfs_restore_inject(file, sbi, &hdr, new_gen);
	if (!err)
		err = agfs_journal_restore(sbi, new_gen, hdr.target_gen);
	/* Don't rollback gen on failure — dirents may already be injected
	 * with new_gen.  Rolling back would leave those dirents with a gen
	 * higher than sbi->gen, breaking COW checks.  The CLI can retry
	 * the operation or abort (which resets gen to 1). */

	up_write(&sbi->staging_sem);

	if (!err) {
		/* Best-effort: restore is already committed to the journal,
		 * so return success even if copy_to_user fails. */
		hdr.new_gen = new_gen;
		if (copy_to_user((void __user *)arg, &hdr, sizeof(hdr)))
			/* new_gen already in journal — userspace can recover */;
	}

	return err;
}

/* ── Unified ioctl handler (rules + ctl) ───────────────────────────── */

long agfs_ioctl(struct file *file, unsigned int cmd, unsigned long arg)
{
	switch (cmd) {
	case AGFS_IOC_GET_REQUEST:
		return agfs_get_request_ioctl(file, arg);

	case AGFS_IOC_PUT_RESPONSE:
		return agfs_put_response_ioctl(file, arg);

	case AGFS_IOC_RULE_ADD:
		return agfs_rule_add_ioctl(file, arg);

	case AGFS_IOC_RULE_REMOVE:
		return agfs_rule_remove_ioctl(file, arg);

	case AGFS_IOC_RESTORE:
		return agfs_restore_ioctl(file, arg);

	case AGFS_IOC_CHECKPOINT:
		return agfs_checkpoint_ioctl(file, arg);

	default:
		return -ENOTTY;
	}
}
