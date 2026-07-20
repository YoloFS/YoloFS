// SPDX-License-Identifier: GPL-2.0
/*
 * yolofs — permission gating layer.
 *
 * Resolve (walk up the dentry chain, live) and check permissions. Implements
 * the ask protocol for blocking threads on unresolved permissions.
 */

#include "yolofs.h"
#include <linux/sched.h>
#include <linux/sched/signal.h>
#include <linux/uaccess.h>

/* ── Resolve permission by walking up dentry chain ─────────────────── */

enum yolo_perm yolo_perm_walk(struct dentry *dentry, struct dentry **source)
{
	struct dentry *cur = dentry;

	if (source)
		*source = NULL;

	while (cur) {
		struct yolo_dentry_info *di = YOLO_D(cur);
		if (di && di->policy != YOLO_PERM_UNSET) {
			if (source)
				*source = dget(cur);
			return di->policy;
		}
		if (cur == cur->d_parent)
			break;
		cur = cur->d_parent;
	}
	return YOLO_PERM_ASK;
}

/* ── Check perm against the attempted op ───────────────────────────── */

static int yolo_perm_check(enum yolo_perm perm, enum yolo_op op)
{
	bool wants_write = op == YOLO_OP_WRITE;

	switch (perm) {
	case YOLO_PERM_ALLOW:
		return 0;
	case YOLO_PERM_WRITE_ASK:
	case YOLO_PERM_READ_ONLY:
		return wants_write ? -EACCES : 0;
	case YOLO_PERM_DENY:
		return -EACCES;
	case YOLO_PERM_ASK:
		return -EACCES; /* ask must be resolved before final checking */
	default:
		return -EACCES;
	}
}

static bool yolo_perm_needs_ask(enum yolo_perm perm, enum yolo_op op)
{
	return perm == YOLO_PERM_ASK ||
	       (perm == YOLO_PERM_WRITE_ASK && op == YOLO_OP_WRITE);
}

/* ── Combined resolve + ask + check ───────────────────────────────── */

/*
 * Apply a static (no-prompt) permission and journal G with result d on deny.
 * @target is the attempted path recorded by G.
 */
static int yolo_perm_check_static(struct yolo_sb_info *sbi,
				  struct dentry *target, enum yolo_perm perm,
				  enum yolo_op op)
{
	int err = yolo_perm_check(perm, op);

	if (err == -EACCES)
		yolo_journal_gate(sbi, target, op, YOLO_GATE_DIRECT_DENY);
	return err;
}

/*
 * Slow path for yolo_perm_check_dentry: the resolved @perm says ask. @source is
 * the rule dentry that resolved it (NULL for the built-in default) — ownership
 * passes here, and we dput it. Prompt the daemon and journal the result.
 * Writes exactly one G for the resolved ask.
 */
static int yolo_perm_ask(struct yolo_sb_info *sbi, struct dentry *source,
			 struct dentry *target, enum yolo_perm perm,
			 enum yolo_op op)
{
	char buf[YOLO_PATH_MAX];
	char rule_buf[YOLO_PATH_MAX];
	char *access_path;
	const char *rule_path = "";
	enum yolo_decision decision;
	int err;

	if (source) {
		rule_path = dentry_path_raw(source, rule_buf, sizeof(rule_buf));
		dput(source);
		if (IS_ERR(rule_path))
			return PTR_ERR(rule_path);
	}

	access_path = dentry_path_raw(target, buf, sizeof(buf));
	if (IS_ERR(access_path))
		return PTR_ERR(access_path);

	err = yolo_ask_userspace(sbi, access_path, rule_path, perm, op,
				 &decision);
	if (err)
		return err;
	yolo_journal_gate(sbi, target, op,
			  decision == YOLO_DECISION_ALLOW ? YOLO_GATE_ASK_ALLOW
							  : YOLO_GATE_ASK_DENY);
	return decision == YOLO_DECISION_ALLOW ? 0 : -EACCES;
}

/*
 * Full permission check: resolve @check's perm with a single walk up the dentry
 * chain, then either ask the daemon (slow path) or apply it statically. Used by
 * yolo_open (via file.c) and metadata ops (via inode.c). The walk captures the
 * source rule dentry (for the ask prompt's rule_path); the ask path consumes
 * it, the static path dputs it.
 *
 * Journaling lives next to the result: a resolved ask writes G on the @target
 * dentry; a static denial writes G in yolo_perm_check_static. Callers just
 * propagate the returned errno. @check is the dentry whose perm gates the
 * access; @target is what G reports (the file itself for opens, the child for
 * parent-gated mutates) — usually the same dentry as @check.
 */
int yolo_perm_check_dentry(struct yolo_sb_info *sbi, struct dentry *check,
			   struct dentry *target, enum yolo_op op)
{
	struct dentry *source = NULL;
	enum yolo_perm perm = yolo_perm_walk(check, &source);

	if (yolo_perm_needs_ask(perm, op))
		return yolo_perm_ask(sbi, source, target, perm, op);

	if (source)
		dput(source);
	return yolo_perm_check_static(sbi, target, perm, op);
}

/* ── Ask Protocol: requester side ─────────────────────────────────── */

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

	req = kzalloc(sizeof(*req), GFP_KERNEL);
	if (!req)
		return -ENOMEM;

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
	/* kzalloc already set decision = DENY (0) */
	init_completion(&req->done);
	INIT_LIST_HEAD(&req->list);

	/* Enqueue and wait for a daemon to answer (or time out -> deny). With no
	 * daemon connected the ask simply waits until prompt_timeout elapses. */
	spin_lock(&sbi->perm.pending_lock);
	list_add_tail(&req->list, &sbi->perm.pending_reqs);
	spin_unlock(&sbi->perm.pending_lock);

	/* Wake any daemon blocked in ASK_PEEK */
	wake_up_interruptible(&sbi->perm.request_waitq);

	/* Wait for decision */
	if (sbi->perm.timeout_ms > 0)
		timeout = msecs_to_jiffies(sbi->perm.timeout_ms);
	else
		timeout = MAX_SCHEDULE_TIMEOUT;
	timeout = wait_for_completion_interruptible_timeout(&req->done,
							    timeout);
	/* Settle under the lock. If the req is still queued, nobody resolved it
	 * (we timed out or were interrupted): default to deny and unlink. If it
	 * is already unlinked, ASK_DECIDE or daemon close resolved it under this
	 * same lock — it set req->decision before unlinking, so keep that. Either
	 * way a racing ASK_DECIDE that arrives after us just won't find it
	 * (ENOENT). kzalloc pre-set req->decision to DENY, so it is always valid
	 * on the success path below. */
	spin_lock(&sbi->perm.pending_lock);
	if (!list_empty(&req->list)) {
		req->decision = YOLO_DECISION_DENY;
		list_del_init(&req->list);
	}
	spin_unlock(&sbi->perm.pending_lock);

	if (timeout < 0)
		err = -EINTR;

	if (!err)
		*result = req->decision;

	kfree(req);
	return err;

out_free:
	kfree(req);
	return err;
}

/* ── Ask Protocol: daemon side (ASK_PEEK / ASK_DECIDE ioctls) ─────────
 *
 * A watcher opens the mount root and loops ASK_PEEK (read the head ask) →
 * decide → ASK_DECIDE (answer by id). Matching by id means late/duplicate
 * answers just return -ENOENT; an unanswered ask is denied on timeout by its
 * own requester in yolo_ask_userspace(). yolo_ctl_ioctl() refuses both ops
 * from inside the mount so nothing gated can answer its own prompts.
 */

/*
 * Resolve a queued ask by id: set its decision, unlink it, and wake the
 * requester. Returns true if the ask was still pending (and is now resolved),
 * false if no such ask is queued (already answered, timed out, or gone).
 *
 * Everything runs under pending_lock, including complete(): the woken
 * requester re-takes pending_lock before freeing the req, so it cannot free it
 * while we still hold a pointer. "On pending_reqs" means "not yet resolved".
 */
static bool yolo_ask_settle(struct yolo_permission *perm, u64 id,
			    enum yolo_decision decision)
{
	struct yolo_ask *req;
	bool found = false;

	spin_lock(&perm->pending_lock);
	list_for_each_entry(req, &perm->pending_reqs, list) {
		if (req->id == id) {
			req->decision = decision;
			list_del_init(&req->list);
			complete(&req->done);
			found = true;
			break;
		}
	}
	spin_unlock(&perm->pending_lock);
	return found;
}

/* ASK_PEEK: copy out the head ask without removing it (it stays queued for
 * ASK_DECIDE to resolve). Blocks for a pending ask unless O_NONBLOCK. */
long yolo_ask_peek_ioctl(struct file *file, unsigned long arg)
{
	struct yolo_sb_info *sbi = YOLO_SB(file_inode(file)->i_sb);
	struct yolo_permission *perm = &sbi->perm;
	struct yolo_ask *req;
	struct yolo_ioc_ask out;
	int err;

	memset(&out, 0, sizeof(out));

	/* Block until a request is pending (unless non-blocking), then take the
	 * lock and re-check — the requester may have reclaimed it meanwhile. */
	if (!(file->f_flags & O_NONBLOCK)) {
		err = wait_event_interruptible(perm->request_waitq,
			!list_empty(&perm->pending_reqs));
		if (err)
			return err;
	}
	spin_lock(&perm->pending_lock);
	if (list_empty(&perm->pending_reqs)) {
		spin_unlock(&perm->pending_lock);
		return -EAGAIN;
	}

	req = list_first_entry(&perm->pending_reqs, struct yolo_ask, list);

	out.id = req->id;
	out.op = req->op;
	out.pid = req->pid;
	strscpy(out.comm, req->comm, sizeof(out.comm));
	out.rule_perm = (__u8)req->rule_perm;
	out.access_path_len = req->access_path_len;
	out.rule_path_len = req->rule_path_len;
	memcpy(out.access_path, req->access_path, req->access_path_len);
	memcpy(out.rule_path, req->rule_path, req->rule_path_len);

	spin_unlock(&perm->pending_lock);

	/* `out` is a private copy, so once we drop the lock we never touch `req`
	 * again on the success path. */
	if (copy_to_user((void __user *)arg, &out, sizeof(out))) {
		/* Bad daemon buffer. Deny this ask by id (it may already be gone
		 * if its requester timed out) so the queue head advances instead
		 * of faulting on the same ask forever. */
		yolo_ask_settle(perm, out.id, YOLO_DECISION_DENY);
		return -EFAULT;
	}

	return 0;
}

/* ASK_DECIDE: answer an ask by id and remove it. */
long yolo_ask_decide_ioctl(struct file *file, unsigned long arg)
{
	struct yolo_permission *perm = &YOLO_SB(file_inode(file)->i_sb)->perm;
	struct yolo_ioc_decision in;

	if (copy_from_user(&in, (void __user *)arg, sizeof(in)))
		return -EFAULT;

	if (in.decision > YOLO_DECISION_ALLOW)
		return -EINVAL;

	return yolo_ask_settle(perm, in.id, (enum yolo_decision)in.decision) ?
		0 : -ENOENT;
}
