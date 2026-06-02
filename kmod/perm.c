// SPDX-License-Identifier: GPL-2.0
/*
 * yolofs — permission gating layer.
 *
 * Resolve, cache, and check permissions. Implements the ask protocol
 * for blocking threads on unresolved permissions.
 */

#include "yolofs.h"
#include <linux/sched.h>
#include <linux/sched/signal.h>

/* ── Resolve permission by walking up dentry chain ─────────────────── */

enum yolo_perm yolo_resolve_perm(struct dentry *dentry)
{
	struct dentry *cur = dentry;

	while (cur) {
		struct yolo_dentry_info *di = YOLO_D(cur);
		if (di && di->perm != YOLO_PERM_UNSET)
			return di->perm;
		if (cur == cur->d_parent)
			break;
		cur = cur->d_parent;
	}
	return YOLO_PERM_ASK;
}

/* ── Cache resolved perm on inode ──────────────────────────────────── */

void yolo_cache_perm(struct inode *inode, struct dentry *dentry)
{
	struct yolo_inode_info *info = YOLO_I(inode);
	struct yolo_sb_info *sbi = YOLO_SB(inode->i_sb);

	info->cached_perm = yolo_resolve_perm(dentry);
	info->perm_gen = atomic64_read(&sbi->perm.gen);
}

/* ── Check perm against file flags ─────────────────────────────────── */

int yolo_check_perm(enum yolo_perm perm, int f_flags)
{
	bool wants_write = (f_flags & (O_WRONLY | O_RDWR | O_APPEND | O_TRUNC));

	switch (perm) {
	case YOLO_PERM_ALLOW:
		return 0;
	case YOLO_PERM_READ:
		return wants_write ? -EACCES : 0;
	case YOLO_PERM_DENY:
		return -EACCES;
	case YOLO_PERM_HIDE:
		return -ENOENT;	/* path doesn't exist from agent's perspective */
	case YOLO_PERM_ASK:
		return 0; /* ask is handled by caller (yolo_open) */
	default:
		return -EACCES;
	}
}

/* Map open flags to the operation being attempted (read vs write). */
enum yolo_op yolo_open_op(int f_flags)
{
	if (f_flags & (O_WRONLY | O_RDWR | O_APPEND | O_TRUNC))
		return YOLO_OP_WRITE;
	return YOLO_OP_READ;
}

/* ── Combined resolve + ask + check ───────────────────────────────── */

/*
 * Full permission check for a dentry: resolve cached perm, ask daemon
 * if unresolved, then check against the given flags.  Used by both
 * yolo_open (via file.c) and metadata ops (via inode.c).
 */
int yolo_check_dentry_perm(struct yolo_sb_info *sbi, struct dentry *dentry,
			   int f_flags)
{
	struct inode *inode = d_inode(dentry);
	struct yolo_inode_info *ii = YOLO_I(inode);
	enum yolo_perm perm;
	int err;

	if (ii->perm_gen != atomic64_read(&sbi->perm.gen))
		yolo_cache_perm(inode, dentry);
	perm = ii->cached_perm;

	if (perm == YOLO_PERM_ASK) {
		enum yolo_op op = yolo_open_op(f_flags);
		char buf[YOLO_PATH_MAX];
		char *relpath;

		relpath = dentry_path_raw(dentry, buf, sizeof(buf));
		if (IS_ERR(relpath))
			return PTR_ERR(relpath);
		err = yolo_ask_userspace(sbi, dentry, relpath, op, &perm);
		if (err)
			return err;
		/* Cache the decision so subsequent checks don't re-ask. */
		ii->cached_perm = perm;
	}

	return yolo_check_perm(perm, f_flags);
}

/* ── Ask Protocol ─────────────────────────────────────────────────── */

int yolo_ask_userspace(struct yolo_sb_info *sbi, struct dentry *dentry,
		       const char *relpath, enum yolo_op op,
		       enum yolo_perm *result)
{
	struct yolo_perm_request *req;
	long timeout;
	int err = 0;

	if (!sbi->perm.enabled) {
		*result = YOLO_PERM_ALLOW;
		return 0;
	}

	/* No daemon connected — deny immediately (an unanswered ask is a deny) */
	if (!atomic_read(&sbi->perm.has_daemon)) {
		*result = YOLO_PERM_DENY;
		yolo_journal_ask(sbi, relpath, op, *result);
		return 0;
	}

	req = kzalloc(sizeof(*req), GFP_KERNEL);
	if (!req)
		return -ENOMEM;

	kref_init(&req->ref);
	req->id = atomic64_inc_return(&sbi->perm.next_req_id);
	req->path_len = strscpy(req->path, relpath, YOLO_PATH_MAX);
	req->op = op;
	req->pid = current->pid;
	get_task_comm(req->comm, current);
	req->decision = YOLO_PERM_UNSET; /* undecided */
	init_completion(&req->done);
	INIT_LIST_HEAD(&req->list);

	/* Enqueue */
	spin_lock(&sbi->perm.pending_lock);
	list_add_tail(&req->list, &sbi->perm.pending_reqs);
	spin_unlock(&sbi->perm.pending_lock);

	/* Wake daemon */
	wake_up_interruptible(&sbi->perm.request_waitq);

	/* Wait for decision */
	if (sbi->perm.timeout_s > 0)
		timeout = msecs_to_jiffies(sbi->perm.timeout_s * 1000);
	else
		timeout = MAX_SCHEDULE_TIMEOUT;
	timeout = wait_for_completion_interruptible_timeout(&req->done,
							    timeout);
	if (timeout == 0)
		req->decision = YOLO_PERM_DENY;
	else if (timeout < 0)
		err = -EINTR;

	/* Remove from pending list if the daemon hasn't dequeued it yet */
	spin_lock(&sbi->perm.pending_lock);
	if (!list_empty(&req->list))
		list_del_init(&req->list);
	spin_unlock(&sbi->perm.pending_lock);

	if (!err && req->decision == YOLO_PERM_UNSET) {
		/* Shouldn't happen — treat as deny */
		req->decision = YOLO_PERM_DENY;
	}

	if (!err) {
		*result = req->decision;
		yolo_journal_ask(sbi, relpath, op, req->decision);
	}

	kref_put(&req->ref, yolo_perm_request_release);
	return err;
}
