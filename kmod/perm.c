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

/* ── Resolve permission by walking up dentry chain (§4.2) ──────────── */

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
	case AGFS_PERM_ASK:
		return 0; /* ask is handled by caller (agfs_open) */
	default:
		return -EACCES;
	}
}

/* ── Ask Protocol (§4.3) ──────────────────────────────────────────── */

int agfs_ask_userspace(struct agfs_sb_info *sbi, struct dentry *dentry,
		       const char *relpath, int f_flags,
		       enum agfs_perm *result)
{
	struct agfs_perm_request *req;
	unsigned int op;
	long timeout;
	int err = 0;

	if (sbi->nogating) {
		*result = AGFS_PERM_ALLOW;
		return 0;
	}

	/* Determine operation type */
	if (f_flags & (O_WRONLY | O_RDWR | O_APPEND | O_TRUNC))
		op = AGFS_OP_WRITE;
	else
		op = AGFS_OP_READ;

	req = kzalloc(sizeof(*req), GFP_KERNEL);
	if (!req)
		return -ENOMEM;

	req->id = atomic64_inc_return(&sbi->next_req_id);
	strscpy(req->path, relpath, AGFS_PATH_MAX);
	req->op = op;
	req->pid = current->pid;
	get_task_comm(req->comm, current);
	req->decision = AGFS_PERM_NONE; /* undecided */
	init_completion(&req->done);
	INIT_LIST_HEAD(&req->list);

	/* Log the ask event */
	agfs_log_emit(sbi, AGFS_LOG_ASK, AGFS_PERM_ASK, op, relpath, req->id);

	/* Enqueue */
	spin_lock(&sbi->pending_lock);
	list_add_tail(&req->list, &sbi->pending_reqs);
	spin_unlock(&sbi->pending_lock);

	/* Wake daemon */
	wake_up_interruptible(&sbi->request_waitq);

	/* Wait for decision */
	if (sbi->ask_timeout_s > 0) {
		timeout = msecs_to_jiffies(sbi->ask_timeout_s * 1000);
		timeout = wait_for_completion_interruptible_timeout(&req->done,
								   timeout);
		if (timeout == 0) {
			/* Timed out — apply default */
			req->decision = sbi->ask_default;
		} else if (timeout < 0) {
			err = -EINTR;
		}
	} else {
		err = wait_for_completion_interruptible(&req->done);
	}

	/* Remove from list if still there */
	spin_lock(&sbi->pending_lock);
	if (!list_empty(&req->list))
		list_del(&req->list);
	spin_unlock(&sbi->pending_lock);

	if (!err && req->decision == AGFS_PERM_NONE) {
		/* Shouldn't happen — treat as deny */
		req->decision = AGFS_PERM_DENY;
	}

	if (!err) {
		*result = req->decision;
		agfs_log_emit(sbi, AGFS_LOG_DECISION, req->decision,
			      op, relpath, req->id);
	}

	kfree(req);
	return err;
}
