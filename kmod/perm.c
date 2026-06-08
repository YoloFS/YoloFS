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

enum yolo_perm yolo_resolve_perm(struct dentry *dentry, struct dentry **source)
{
	struct dentry *cur = dentry;

	if (source)
		*source = NULL;

	while (cur) {
		struct yolo_dentry_info *di = YOLO_D(cur);
		if (di && di->perm != YOLO_PERM_UNSET) {
			if (source)
				*source = dget(cur);
			return di->perm;
		}
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

	info->cached_perm = yolo_resolve_perm(dentry, NULL);
	info->perm_gen = atomic64_read(&sbi->perm.gen);
}

/* ── Check perm against file flags ─────────────────────────────────── */

int yolo_check_perm(enum yolo_perm perm, int f_flags)
{
	bool wants_write = yolo_open_op(f_flags) == YOLO_OP_WRITE;

	switch (perm) {
	case YOLO_PERM_ALLOW:
		return 0;
	case YOLO_PERM_WRITE_ASK:
		return wants_write ? -EACCES : 0;
	case YOLO_PERM_READ_ONLY:
		return wants_write ? -EACCES : 0;
	case YOLO_PERM_DENY:
		return -EACCES;
	case YOLO_PERM_HIDE:
		return -ENOENT;	/* path doesn't exist from agent's perspective */
	case YOLO_PERM_ASK:
		return -EACCES; /* ask must be resolved before final checking */
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

static bool yolo_perm_needs_ask(enum yolo_perm perm, enum yolo_op op)
{
	return perm == YOLO_PERM_ASK ||
	       (perm == YOLO_PERM_WRITE_ASK && op == YOLO_OP_WRITE);
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
	enum yolo_decision decision;
	enum yolo_op op = yolo_open_op(f_flags);
	int err;

	if (ii->perm_gen != atomic64_read(&sbi->perm.gen))
		yolo_cache_perm(inode, dentry);
	perm = ii->cached_perm;

	if (yolo_perm_needs_ask(perm, op)) {
		char buf[YOLO_PATH_MAX];
		char rule_buf[YOLO_PATH_MAX];
		char *relpath;
		const char *rule_path = "";
		struct dentry *source = NULL;

		relpath = dentry_path_raw(dentry, buf, sizeof(buf));
		if (IS_ERR(relpath))
			return PTR_ERR(relpath);

		perm = yolo_resolve_perm(dentry, &source);
		ii->cached_perm = perm;
		ii->perm_gen = atomic64_read(&sbi->perm.gen);
		if (!yolo_perm_needs_ask(perm, op)) {
			if (source)
				dput(source);
			return yolo_check_perm(perm, f_flags);
		}

		if (source) {
			rule_path = dentry_path_raw(source, rule_buf,
						   sizeof(rule_buf));
			dput(source);
			if (IS_ERR(rule_path))
				return PTR_ERR(rule_path);
		}

		err = yolo_ask_userspace(sbi, relpath, rule_path, perm, op,
					 &decision);
		if (err)
			return err;
		return decision == YOLO_DECISION_ALLOW ? 0 : -EACCES;
	}

	return yolo_check_perm(perm, f_flags);
}

/* ── Ask Protocol ─────────────────────────────────────────────────── */

static int yolo_store_ask_path(char *dst, u16 *dst_len, const char *src)
{
	ssize_t copied = strscpy(dst, src, YOLO_PATH_MAX);

	if (copied < 0)
		return copied;
	*dst_len = (u16)copied;
	return 0;
}

int yolo_ask_userspace(struct yolo_sb_info *sbi, const char *access_path,
		       const char *rule_path, enum yolo_perm rule_perm,
		       enum yolo_op op, enum yolo_decision *result)
{
	struct yolo_ask *req;
	long timeout;
	int err = 0;

	if (!sbi->perm.enabled) {
		*result = YOLO_DECISION_ALLOW;
		return 0;
	}

	/* No daemon connected — deny immediately (an unanswered ask is a deny) */
	if (!READ_ONCE(sbi->perm.daemon_file)) {
		*result = YOLO_DECISION_DENY;
		yolo_journal_ask(sbi, access_path, op, *result);
		return 0;
	}

	req = kzalloc(sizeof(*req), GFP_KERNEL);
	if (!req)
		return -ENOMEM;

	kref_init(&req->ref);
	req->id = atomic64_inc_return(&sbi->perm.next_req_id);
	err = yolo_store_ask_path(req->access_path, &req->access_path_len,
				  access_path);
	if (err)
		goto out_free;
	err = yolo_store_ask_path(req->rule_path, &req->rule_path_len,
				  rule_path);
	if (err)
		goto out_free;
	req->rule_perm = rule_perm;
	req->op = op;
	req->pid = current->pid;
	get_task_comm(req->comm, current);
	req->decision = YOLO_DECISION_DENY;
	req->decided = false;
	req->dispatched = false;
	init_completion(&req->done);
	INIT_LIST_HEAD(&req->list);

	/* Enqueue */
	spin_lock(&sbi->perm.pending_lock);
	if (!READ_ONCE(sbi->perm.daemon_file)) {
		spin_unlock(&sbi->perm.pending_lock);
		*result = YOLO_DECISION_DENY;
		yolo_journal_ask(sbi, access_path, op, *result);
		kref_put(&req->ref, yolo_ask_release);
		return 0;
	}
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
	/* Settle under the lock: on timeout, default to deny unless the daemon
	 * already answered; in every case, reclaim the req if it is still
	 * pending. If the daemon has moved it to the dispatched list, leave it
	 * there — PUT_DECISION or daemon cleanup owns it now and drops the
	 * dispatched reference. */
	spin_lock(&sbi->perm.pending_lock);
	if (timeout == 0 && !req->decided) {
		req->decision = YOLO_DECISION_DENY;
		req->decided = true;
	}
	if (!req->dispatched && !list_empty(&req->list))
		list_del_init(&req->list);
	spin_unlock(&sbi->perm.pending_lock);

	if (timeout < 0)
		err = -EINTR;

	if (!err && !req->decided) {
		/* Shouldn't happen — treat as deny */
		req->decision = YOLO_DECISION_DENY;
		req->decided = true;
	}

	if (!err) {
		*result = req->decision;
		yolo_journal_ask(sbi, access_path, op, req->decision);
	}

	kref_put(&req->ref, yolo_ask_release);
	return err;

out_free:
	kfree(req);
	return err;
}
