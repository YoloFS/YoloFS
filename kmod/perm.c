// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — permission gating layer.
 *
 * Resolve, cache, and check permissions. Implements the ask protocol
 * for blocking threads on unresolved permissions.
 */

#include "agfs.h"
#include <linux/sched.h>
#include <linux/sched/signal.h>

/* ── Resolve permission by walking up dentry chain ─────────────────── */

enum agfs_perm agfs_resolve_perm(struct dentry *dentry)
{
	struct dentry *cur = dentry;

	while (cur) {
		struct agfs_dentry_info *di = AGFS_D(cur);
		if (di && di->perm != AGFS_PERM_NONE)
			return di->perm;
		if (cur == cur->d_parent)
			break;
		cur = cur->d_parent;
	}
	return AGFS_PERM_ASK;
}

/* ── Cache resolved perm on inode ──────────────────────────────────── */

void agfs_cache_perm(struct inode *inode, struct dentry *dentry)
{
	struct agfs_inode_info *info = AGFS_I(inode);
	struct agfs_sb_info *sbi = AGFS_SB(inode->i_sb);

	info->cached_perm = agfs_resolve_perm(dentry);
	info->perm_gen = atomic64_read(&sbi->perm_gen);
}

/* ── Check perm against file flags ─────────────────────────────────── */

int agfs_check_perm(enum agfs_perm perm, int f_flags)
{
	bool wants_write = (f_flags & (O_WRONLY | O_RDWR | O_APPEND | O_TRUNC));

	switch (perm) {
	case AGFS_PERM_ALLOW:
		return 0;
	case AGFS_PERM_ALLOW_RW:
		return 0;
	case AGFS_PERM_ALLOW_RO:
		return wants_write ? -EACCES : 0;
	case AGFS_PERM_ALLOW_RX:
		return wants_write ? -EACCES : 0;
	case AGFS_PERM_DENY:
		return -EACCES;
	case AGFS_PERM_HIDE:
		return -ENOENT;	/* path doesn't exist from agent's perspective */
	case AGFS_PERM_ASK:
		return 0; /* ask is handled by caller (agfs_open) */
	default:
		return -EACCES;
	}
}

/* ── Combined resolve + ask + check ───────────────────────────────── */

/*
 * Full permission check for a dentry: resolve cached perm, ask daemon
 * if unresolved, then check against the given flags.  Used by both
 * agfs_open (via file.c) and metadata ops (via inode.c).
 */
int agfs_check_dentry_perm(struct agfs_sb_info *sbi, struct dentry *dentry,
			   int f_flags, fmode_t f_mode)
{
	struct inode *inode = d_inode(dentry);
	struct agfs_inode_info *ii = AGFS_I(inode);
	enum agfs_perm perm;
	int err;

	if (ii->perm_gen != atomic64_read(&sbi->perm_gen))
		agfs_cache_perm(inode, dentry);
	perm = ii->cached_perm;

	if (perm == AGFS_PERM_ASK) {
		unsigned int op;
		char buf[AGFS_PATH_MAX];
		char *relpath;

		if (f_mode & FMODE_EXEC)
			op = AGFS_OP_EXEC;
		else if (f_flags & (O_WRONLY | O_RDWR | O_APPEND | O_TRUNC))
			op = AGFS_OP_WRITE;
		else
			op = AGFS_OP_READ;

		relpath = dentry_path_raw(dentry, buf, sizeof(buf));
		if (IS_ERR(relpath))
			return PTR_ERR(relpath);
		err = agfs_ask_userspace(sbi, dentry, relpath, op, &perm);
		if (err)
			return err;
	}

	return agfs_check_perm(perm, f_flags);
}

int agfs_check_dir_perm(struct agfs_sb_info *sbi, struct dentry *dentry,
			bool write, bool exec)
{
	int err;
	int f_flags = write ? O_WRONLY : O_RDONLY;
	fmode_t f_mode = exec ? FMODE_EXEC : 0;

	err = agfs_check_dentry_perm(sbi, dentry, f_flags, f_mode);
	if (!write && err == -EACCES)
		return 0;
	return err;
}

/* ── Ask Protocol ─────────────────────────────────────────────────── */

int agfs_ask_userspace(struct agfs_sb_info *sbi, struct dentry *dentry,
		       const char *relpath, enum agfs_op op,
		       enum agfs_perm *result)
{
	struct agfs_perm_request *req;
	long timeout;
	int err = 0;

	if (!sbi->permission) {
		*result = AGFS_PERM_ALLOW;
		return 0;
	}

	/* No daemon connected — apply default immediately */
	if (!atomic_read(&sbi->ask_engine.has_daemon)) {
		*result = sbi->ask_engine.default_perm;
		return 0;
	}

	req = kzalloc(sizeof(*req), GFP_KERNEL);
	if (!req)
		return -ENOMEM;

	kref_init(&req->ref);
	req->id = atomic64_inc_return(&sbi->ask_engine.next_req_id);
	req->path_len = strscpy(req->path, relpath, AGFS_PATH_MAX);
	req->op = op;
	req->pid = current->pid;
	get_task_comm(req->comm, current);
	req->decision = AGFS_PERM_NONE; /* undecided */
	init_completion(&req->done);
	INIT_LIST_HEAD(&req->list);

	/* Enqueue */
	spin_lock(&sbi->ask_engine.pending_lock);
	list_add_tail(&req->list, &sbi->ask_engine.pending_reqs);
	spin_unlock(&sbi->ask_engine.pending_lock);

	/* Wake daemon */
	wake_up_interruptible(&sbi->ask_engine.request_waitq);

	/* Wait for decision */
	if (sbi->ask_engine.timeout_s > 0)
		timeout = msecs_to_jiffies(sbi->ask_engine.timeout_s * 1000);
	else
		timeout = MAX_SCHEDULE_TIMEOUT;
	timeout = wait_for_completion_interruptible_timeout(&req->done,
							    timeout);
	if (timeout == 0)
		req->decision = sbi->ask_engine.default_perm;
	else if (timeout < 0)
		err = -EINTR;

	/* Remove from pending list if the daemon hasn't dequeued it yet */
	spin_lock(&sbi->ask_engine.pending_lock);
	if (!list_empty(&req->list))
		list_del_init(&req->list);
	spin_unlock(&sbi->ask_engine.pending_lock);

	if (!err && req->decision == AGFS_PERM_NONE) {
		/* Shouldn't happen — treat as deny */
		req->decision = AGFS_PERM_DENY;
	}

	if (!err)
		*result = req->decision;

	kref_put(&req->ref, agfs_perm_request_release);
	return err;
}
