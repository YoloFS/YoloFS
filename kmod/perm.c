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

/* ── Check perm against file flags ─────────────────────────────────── */

/* Map open flags to the operation being attempted (read vs write). */
static enum yolo_op yolo_open_op(int f_flags)
{
	if (f_flags & (O_WRONLY | O_RDWR | O_APPEND | O_TRUNC))
		return YOLO_OP_WRITE;
	return YOLO_OP_READ;
}

static int yolo_perm_check(enum yolo_perm perm, int f_flags)
{
	bool wants_write = yolo_open_op(f_flags) == YOLO_OP_WRITE;

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
				  int f_flags)
{
	int err = yolo_perm_check(perm, f_flags);

	if (err == -EACCES)
		yolo_journal_gate(sbi, target, yolo_open_op(f_flags),
				  YOLO_GATE_DIRECT_DENY);
	return err;
}

/*
 * Slow path for yolo_perm_check_dentry: the resolved perm says ask. Re-walk to
 * pin the source rule, prompt the daemon, and journal the result — unless the
 * re-walk finds the perm is now static (a racing rule update), in which case
 * fall through to the static check. Writes exactly one G for the resolved ask.
 */
static int yolo_perm_ask(struct yolo_sb_info *sbi, struct dentry *check,
			 struct dentry *target, enum yolo_op op, int f_flags)
{
	char buf[YOLO_PATH_MAX];
	char rule_buf[YOLO_PATH_MAX];
	char *access_path;
	const char *rule_path = "";
	struct dentry *source = NULL;
	enum yolo_decision decision;
	enum yolo_perm perm;
	int err;

	perm = yolo_perm_walk(check, &source);
	if (!yolo_perm_needs_ask(perm, op)) {
		/* Race: rules changed and it is now a static perm. */
		if (source)
			dput(source);
		return yolo_perm_check_static(sbi, target, perm, f_flags);
	}

	if (source) {
		rule_path = dentry_path_raw(source, rule_buf, sizeof(rule_buf));
		dput(source);
		if (IS_ERR(rule_path))
			return PTR_ERR(rule_path);
	}

	/* Resolve the target path only now that we know we will ask. A race
	 * that flipped this to a static perm above took the static path, which
	 * resolves its own note path. */
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
 * Full permission check: resolve @check's perm (walk up the dentry chain) and
 * either ask the daemon (slow path) or apply it statically. Used by yolo_open
 * (via file.c) and metadata ops (via inode.c).
 *
 * Journaling lives next to the result: a resolved ask writes G on the @target
 * dentry; a static denial writes G in yolo_perm_check_static. Callers just
 * propagate the returned errno. @check is the dentry whose perm gates the
 * access; @target is what G reports (the file itself for opens, the child for
 * parent-gated mutates) — usually the same dentry as @check.
 */
int yolo_perm_check_dentry(struct yolo_sb_info *sbi, struct dentry *check,
			   struct dentry *target, int f_flags)
{
	enum yolo_op op = yolo_open_op(f_flags);
	enum yolo_perm perm = yolo_perm_walk(check, NULL);

	if (yolo_perm_needs_ask(perm, op))
		return yolo_perm_ask(sbi, check, target, op, f_flags);

	return yolo_perm_check_static(sbi, target, perm, f_flags);
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
