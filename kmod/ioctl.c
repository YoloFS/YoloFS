// SPDX-License-Identifier: GPL-2.0
/*
 * yolofs — control interface (ioctls on the mount-root directory).
 *
 * All control operations are ioctls on a directory fd in the mount (the CLI
 * opens the mount root; commands run inside the mount open "."). Only the
 * owning uid (or CAP_SYS_ADMIN) may issue them. The permission daemon claims
 * exclusive status on its first GET_ASK call; only that fd may issue GET_ASK
 * and PUT_DECISION. All other operations may be issued from any fd.
 *
 * On close of the daemon fd, any dispatched-but-unanswered requests get the
 * default decision.
 */

#include "yolofs.h"
#include <linux/file.h>
#include <linux/fs_struct.h>
#include <linux/vmalloc.h>

/* True if the caller is chrooted into this mount (an agent command or the
 * interactive `yolo` shell), as opposed to a normal terminal outside it.
 * Gating-defeating ops are refused from inside so nothing inside the mount can
 * un-gate itself or answer its own ask prompts. */
static bool yolo_caller_inside(struct super_block *sb)
{
	struct path root;
	bool inside;

	get_fs_root(current->fs, &root);
	inside = (root.dentry->d_sb == sb);
	path_put(&root);
	return inside;
}

/* ── Daemon release ────────────────────────────────────────────────── */

/* Deny every request on @head. Runs under pending_lock — completing under the
 * lock serializes against the requester's settle path. @put_ref drops the
 * extra reference carried by entries on the dispatched list. */
static void yolo_deny_reqs(struct list_head *head, bool put_ref)
{
	struct yolo_ask *req, *tmp;

	list_for_each_entry_safe(req, tmp, head, list) {
		req->decision = YOLO_DECISION_DENY;
		req->decided = true;
		list_del_init(&req->list);
		req->dispatched = false;
		complete(&req->done);
		if (put_ref)
			kref_put(&req->ref, yolo_ask_release);
	}
}

void yolo_ctl_release(struct file *file)
{
	struct yolo_sb_info *sbi = YOLO_SB(file_inode(file)->i_sb);
	struct yolo_permission *perm = &sbi->perm;

	spin_lock(&perm->pending_lock);
	if (perm->daemon_file == file) {
		yolo_deny_reqs(&perm->pending_reqs, false);
		yolo_deny_reqs(&perm->dispatched, true);
		perm->daemon_file = NULL;
	}
	spin_unlock(&perm->pending_lock);
}

/* ── GET_ASK: dequeue pending request ──────────────────────────── */

static long yolo_get_ask_ioctl(struct file *file, unsigned long arg)
{
	struct yolo_sb_info *sbi = YOLO_SB(file_inode(file)->i_sb);
	struct yolo_permission *perm = &sbi->perm;
	struct yolo_ask *req;
	struct yolo_ioc_ask out;
	int err;

	/* Claim daemon status on first call; reject if another fd already has it.
	 * Tracked by file identity (private_data holds the dir's readdir cursor). */
	spin_lock(&perm->pending_lock);
	if (perm->daemon_file != file && perm->daemon_file != NULL) {
		spin_unlock(&perm->pending_lock);
		return -EBUSY;
	}
	if (perm->daemon_file == NULL)
		perm->daemon_file = file;
	spin_unlock(&perm->pending_lock);

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

	/* Hand off to the daemon: move pending -> dispatched and take a
	 * reference, all under pending_lock (which guards both lists). The
	 * reference keeps the req alive across the copies below; it is dropped
	 * by PUT_DECISION / daemon cleanup when they remove it from dispatched. */
	list_move_tail(&req->list, &perm->dispatched);
	req->dispatched = true;
	kref_get(&req->ref);
	spin_unlock(&perm->pending_lock);

	/* Write header back to userspace */
	if (copy_to_user((void __user *)arg, &out, sizeof(out))) {
		err = -EFAULT;
		goto deny;
	}

	return 0;

deny:
	/* Delivery failed (daemon passed a bad buffer). Deny this one req
	 * rather than requeue it — a retry would fault on the same buffer. */
	spin_lock(&perm->pending_lock);
	list_del_init(&req->list);
	req->dispatched = false;
	spin_unlock(&perm->pending_lock);
	req->decision = YOLO_DECISION_DENY;
	req->decided = true;
	complete(&req->done);
	kref_put(&req->ref, yolo_ask_release);
	return err;
}

/* ── PUT_DECISION: submit decision ─────────────────────────────────── */

static long yolo_put_decision_ioctl(struct file *file, unsigned long arg)
{
	struct yolo_permission *perm = &YOLO_SB(file_inode(file)->i_sb)->perm;
	struct yolo_ioc_decision in;
	struct yolo_ask *req;
	bool found = false;
	bool stale = false;

	if (copy_from_user(&in, (void __user *)arg, sizeof(in)))
		return -EFAULT;

	if (in.decision > YOLO_DECISION_ALLOW)
		return -EINVAL;

	spin_lock(&perm->pending_lock);
	if (perm->daemon_file != file) {
		spin_unlock(&perm->pending_lock);
		return -EPERM;
	}
	list_for_each_entry(req, &perm->dispatched, list) {
		if (req->id == in.id) {
			list_del_init(&req->list);
			req->dispatched = false;
			if (req->decided) {
				stale = true;
			} else {
				req->decision = (enum yolo_decision)in.decision;
				req->decided = true;
				found = true;
			}
			break;
		}
	}
	spin_unlock(&perm->pending_lock);

	if (!found && !stale)
		return -ENOENT;

	if (found)
		complete(&req->done);
	kref_put(&req->ref, yolo_ask_release);
	return found ? 0 : -ENOENT;
}

/* ── Release all rule-pinned dentries ───────────────────────────────── */

void yolo_release_pinned_rules(struct yolo_sb_info *sbi)
{
	LIST_HEAD(local);
	struct yolo_dentry_info *di, *tmp;

	spin_lock(&sbi->perm.pinned_rules_lock);
	list_splice_init(&sbi->perm.pinned_rules, &local);
	spin_unlock(&sbi->perm.pinned_rules_lock);

	list_for_each_entry_safe(di, tmp, &local, rule_pin) {
		struct dentry *dentry = di->rule_dentry;

		list_del_init(&di->rule_pin);
		spin_lock(&di->lock);
		di->perm = YOLO_PERM_UNSET;
		di->rule_dentry = NULL;
		spin_unlock(&di->lock);
		dput(dentry);
	}
}

/* ── String copy helper ────────────────────────────────────────────── */

/*
 * Copy a variable-length string from userspace into a caller-provided buffer.
 * The buffer must be at least YOLO_PATH_MAX bytes; values are limited to
 * YOLO_PATH_MAX-1 bytes (same as internal buffers). Used by SNAPSHOT for the
 * snapshot name — rule targets are passed as fds, not strings.
 */
static int yolo_copy_user_path(__u64 ptr, __u16 len, char *buf)
{
	if (!ptr || len == 0 || len >= YOLO_PATH_MAX)
		return -EINVAL;

	if (copy_from_user(buf, (const void __user *)ptr, len))
		return -EFAULT;
	buf[len] = '\0';

	return 0;
}

/*
 * Resolve the rule target from the O_PATH fd in the ioctl payload. The fd was
 * opened by the CLI through the mount, so the path walk already happened in
 * userspace; here we only validate the object: it must live on this mount
 * (-EXDEV) and still be reachable by name (-EINVAL on unlinked — an fd can
 * outlive its path, which kern_path never produced; a rule there would pin a
 * dentry no lookup reaches). On success *rule_path holds its own reference,
 * independent of the fd; callers drop it with path_put().
 */
static int yolo_resolve_rule(struct file *file, unsigned long arg,
			     struct yolo_ioc_rule *rule,
			     struct path *rule_path,
			     struct yolo_dentry_info **di_out)
{
	struct file *target;
	int err = 0;

	if (copy_from_user(rule, (void __user *)arg, sizeof(*rule)))
		return -EFAULT;

	/* fget_raw, not fget: plain fget masks out O_PATH (FMODE_PATH) files,
	 * and the CLI opens rule targets with O_PATH (fdget_raw would do, but
	 * its __fdget_raw helper is not exported to modules). */
	target = fget_raw(rule->fd);
	if (!target)
		return -EBADF;

	if (target->f_path.dentry->d_sb != file_inode(file)->i_sb)
		err = -EXDEV;
	else if (d_unlinked(target->f_path.dentry))
		err = -EINVAL;
	else if (!YOLO_D(target->f_path.dentry))
		err = -ENOENT;

	if (!err) {
		*rule_path = target->f_path;
		path_get(rule_path);
		*di_out = YOLO_D(rule_path->dentry);
	}
	fput(target);
	return err;
}

/* ── Rule / snapshot ioctl handlers ─────────────────────────────────── */

/*
 * YOLO_IOC_RULE_SET: attach a rule to a path's dentry. perm == YOLO_PERM_UNSET
 * clears the rule (it reverts to inheriting from ancestors); any other perm
 * sets it. Pins the dentry on first attach, unpins on clear.
 */
static long yolo_rule_set_ioctl(struct file *file, unsigned long arg)
{
	struct yolo_sb_info *sbi = YOLO_SB(file_inode(file)->i_sb);
	struct yolo_ioc_rule rule;
	struct path rule_path;
	struct yolo_dentry_info *di;
	int err;

	err = yolo_resolve_rule(file, arg, &rule, &rule_path, &di);
	if (err)
		return err;

	if (rule.perm > YOLO_PERM_HIDE) {
		path_put(&rule_path);
		return -EINVAL;
	}

	if (rule.perm == YOLO_PERM_UNSET) {
		bool had_rule;

		spin_lock(&di->lock);
		had_rule = (di->perm != YOLO_PERM_UNSET);
		if (had_rule) {
			di->perm = YOLO_PERM_UNSET;
			di->rule_dentry = NULL;
		}
		spin_unlock(&di->lock);

		if (had_rule) {
			spin_lock(&sbi->perm.pinned_rules_lock);
			if (!list_empty(&di->rule_pin))
				list_del_init(&di->rule_pin);
			spin_unlock(&sbi->perm.pinned_rules_lock);
			dput(rule_path.dentry);
		}
	} else {
		bool first;

		spin_lock(&di->lock);
		first = (di->perm == YOLO_PERM_UNSET);
		if (first) {
			dget(rule_path.dentry);
			di->rule_dentry = rule_path.dentry;
		}
		di->perm = (enum yolo_perm)rule.perm;
		spin_unlock(&di->lock);

		if (first) {
			spin_lock(&sbi->perm.pinned_rules_lock);
			if (list_empty(&di->rule_pin))
				list_add(&di->rule_pin, &sbi->perm.pinned_rules);
			spin_unlock(&sbi->perm.pinned_rules_lock);
		}
	}

	atomic64_inc(&sbi->perm.gen);
	path_put(&rule_path);
	return 0;
}

/*
 * YOLO_IOC_RULE_RESOLVE: report the effective perm for a path by walking up to
 * the nearest ancestor rule (the same resolution the kernel enforces). Writes
 * the resolved perm back into rule.perm.
 */
static long yolo_rule_resolve_ioctl(struct file *file, unsigned long arg)
{
	struct yolo_ioc_rule rule;
	struct path rule_path;
	struct yolo_dentry_info *di;
	int err;

	err = yolo_resolve_rule(file, arg, &rule, &rule_path, &di);
	if (err)
		return err;

	rule.perm = (__u8)yolo_perm_walk(rule_path.dentry, NULL);
	path_put(&rule_path);

	if (copy_to_user((void __user *)arg, &rule, sizeof(rule)))
		return -EFAULT;
	return 0;
}

static long yolo_snapshot_ioctl(struct file *file, unsigned long arg)
{
	struct yolo_sb_info *sbi = YOLO_SB(file_inode(file)->i_sb);
	struct yolo_ioc_snapshot snap;
	char name_buf[YOLO_PATH_MAX];
	u16 gen;
	int err;

	if (!sbi->staging.enabled)
		return -EOPNOTSUPP;

	if (copy_from_user(&snap, (void __user *)arg, sizeof(snap)))
		return -EFAULT;

	err = yolo_copy_user_path(snap.name_ptr, snap.name_len, name_buf);
	if (err)
		return err;

	down_write(&sbi->staging.sem);
	if (atomic_read(&sbi->staging.fd_count) > 0) {
		up_write(&sbi->staging.sem);
		return -EBUSY;
	}
	if ((snap.flags & YOLO_SNAPSHOT_IF_CHANGED) && !READ_ONCE(sbi->staging.dirty)) {
		up_write(&sbi->staging.sem);
		snap.gen = 0;
		if (copy_to_user((void __user *)arg, &snap, sizeof(snap)))
			return -EFAULT;
		return 0;
	}
	if (atomic_read(&sbi->staging.gen) >= U16_MAX) {
		up_write(&sbi->staging.sem);
		return -EOVERFLOW;
	}
	gen = (u16)atomic_inc_return(&sbi->staging.gen);
	yolo_journal_snapshot(sbi, name_buf);
	WRITE_ONCE(sbi->staging.dirty, false);
	up_write(&sbi->staging.sem);

	/* Best-effort: snapshot is already committed to the journal,
	 * so return success even if copy_to_user fails. */
	snap.gen = gen;
	if (copy_to_user((void __user *)arg, &snap, sizeof(snap)))
		/* gen already in journal — userspace can read it back */;

	return 0;
}

/* ── Travel ioctl handler ────────────────────────────────────────────── */

/* ── Cursor helpers for reading the serialized DirTree buffer ──────── */

struct tree_cursor {
	const u8 *buf;
	const u8 *end;
};

static inline int read_u8(struct tree_cursor *c, u8 *out)
{
	if (c->buf + 1 > c->end)
		return -EINVAL;
	*out = *c->buf++;
	return 0;
}

static inline int read_le16(struct tree_cursor *c, u16 *out)
{
	if (c->buf + 2 > c->end)
		return -EINVAL;
	*out = (u16)c->buf[0] | ((u16)c->buf[1] << 8);
	c->buf += 2;
	return 0;
}

static inline int read_le32(struct tree_cursor *c, u32 *out)
{
	if (c->buf + 4 > c->end)
		return -EINVAL;
	*out = (u32)c->buf[0] | ((u32)c->buf[1] << 8) |
	       ((u32)c->buf[2] << 16) | ((u32)c->buf[3] << 24);
	c->buf += 4;
	return 0;
}

static inline int read_bytes(struct tree_cursor *c, u16 len, const u8 **out)
{
	if (c->buf + len > c->end)
		return -EINVAL;
	*out = c->buf;
	c->buf += len;
	return 0;
}

struct dir_frame {
	struct dentry *dentry;
	u16 remaining;
};

/*
 * Parse the target-specific payload from @cur and inject the corresponding
 * dentry under @parent. Scaffold entries (TARGET_PATH with path_len == 0)
 * have no overlay state and create no dentry.
 *
 * Returns the injected dentry (borrowed — its ref is the staging pin), NULL
 * for a scaffold, or ERR_PTR. Staged inodes are stamped by @cow_ino_floor:
 * above it (live-segment edits) at @gen, write-in-place; at or below it
 * (snapshot-retained content) one generation behind, so the first write
 * re-COWs.
 */
static struct dentry *travel_inject_entry(struct tree_cursor *cur,
					  struct yolo_sb_info *sbi,
					  struct dentry *parent,
					  const u8 *name_ptr, u16 name_len,
					  u8 target, u16 gen,
					  u32 cow_ino_floor)
{
	struct path lower_path;
	struct dentry *child;
	int err;

	switch (target) {
	case YOLO_TARGET_NONE: /* tombstone */
		return yolo_dentry_create(parent, (const char *)name_ptr,
					  name_len, YOLO_TARGET_NONE, NULL);

	case YOLO_TARGET_INODE: { /* staged inode */
		u32 ino;

		err = read_le32(cur, &ino);
		if (err)
			return ERR_PTR(err);
		err = yolo_inode_path(sbi, ino, &lower_path);
		if (err)
			return ERR_PTR(err);
		child = yolo_dentry_create(parent, (const char *)name_ptr,
					   name_len, YOLO_TARGET_INODE,
					   &lower_path);
		if (IS_ERR(child))
			return child;
		YOLO_I(d_inode(child))->staging_gen =
			(ino > cow_ino_floor) ? gen : (gen ? gen - 1 : 0);
		YOLO_I(d_inode(child))->staging_ino = ino;
		return child;
	}

	case YOLO_TARGET_PATH: { /* redirect, or passthrough if path_len == 0 */
		char path_buf[YOLO_PATH_MAX];
		const u8 *base_ptr;
		u16 base_len;

		err = read_le16(cur, &base_len);
		if (err)
			return ERR_PTR(err);

		if (base_len == 0) /* passthrough scaffold — no state to set */
			return NULL;

		if (base_len >= YOLO_PATH_MAX)
			return ERR_PTR(-EINVAL);
		err = read_bytes(cur, base_len, &base_ptr);
		if (err)
			return ERR_PTR(err);

		memcpy(path_buf, base_ptr, base_len);
		path_buf[base_len] = '\0';
		err = kern_path(path_buf, LOOKUP_FOLLOW, &lower_path);
		if (err)
			return ERR_PTR(err);
		return yolo_dentry_create(parent, (const char *)name_ptr,
					  name_len, YOLO_TARGET_PATH,
					  &lower_path);
	}

	default:
		return ERR_PTR(-EINVAL);
	}
}

static int yolo_view_inject(struct file *file, struct yolo_sb_info *sbi,
			    u64 tree_ptr, u64 tree_len, u16 gen,
			    u32 cow_ino_floor)
{
	struct dir_frame stack[YOLO_TRAVEL_MAX_DEPTH];
	struct tree_cursor cur;
	u8 *kbuf;
	int depth;
	int err = 0;
	u16 root_count;

	if (tree_len > YOLO_TRAVEL_MAX_TREE_LEN)
		return -EINVAL;

	kbuf = vmalloc(tree_len);
	if (!kbuf)
		return -ENOMEM;

	if (copy_from_user(kbuf, (const void __user *)tree_ptr, tree_len)) {
		vfree(kbuf);
		return -EFAULT;
	}

	cur.buf = kbuf;
	cur.end = kbuf + tree_len;

	/* Read root child_count */
	err = read_le16(&cur, &root_count);
	if (err)
		goto out_free;

	if (root_count == 0)
		goto check_trailing;

	stack[0].dentry = dget(file_inode(file)->i_sb->s_root);
	stack[0].remaining = root_count;
	depth = 0;

	while (depth >= 0) {
		u16 name_len, child_count;
		const u8 *name_ptr;
		struct dentry *child;
		u8 target;

		if (stack[depth].remaining == 0) {
			dput(stack[depth].dentry);
			depth--;
			continue;
		}
		stack[depth].remaining--;

		/* Read name */
		err = read_le16(&cur, &name_len);
		if (err)
			goto out_unwind;
		if (name_len == 0 || name_len > NAME_MAX) {
			err = -EINVAL;
			goto out_unwind;
		}
		err = read_bytes(&cur, name_len, &name_ptr);
		if (err)
			goto out_unwind;

		/* Read and handle entry target */
		err = read_u8(&cur, &target);
		if (err)
			goto out_unwind;
		child = travel_inject_entry(&cur, sbi, stack[depth].dentry,
					    name_ptr, name_len, target, gen,
					    cow_ino_floor);
		if (IS_ERR(child)) {
			err = PTR_ERR(child);
			goto out_unwind;
		}

		/* Read child_count */
		err = read_le16(&cur, &child_count);
		if (err)
			goto out_unwind;

		/* Skip nodes with no children to descend into. */
		if (child_count == 0)
			continue;

		if (depth + 1 >= YOLO_TRAVEL_MAX_DEPTH) {
			err = -EINVAL;
			goto out_unwind;
		}
		if (child) {
			/* Descend into the dentry just injected. */
			dget(child);
		} else {
			/* Scaffold — the name resolves through base. */
			child = yolo_lower_lookup_unlocked(
					mnt_idmap(file->f_path.mnt),
					stack[depth].dentry,
					(const char *)name_ptr, name_len);
			if (IS_ERR(child)) {
				err = PTR_ERR(child);
				goto out_unwind;
			}
		}
		depth++;
		stack[depth].dentry = child;
		stack[depth].remaining = child_count;
	}

check_trailing:
	if (cur.buf != cur.end)
		err = -EINVAL;
	goto out_free;

out_unwind:
	/* Clean up stacked dentries */
	while (depth >= 0) {
		dput(stack[depth].dentry);
		depth--;
	}
out_free:
	vfree(kbuf);
	return err;
}

/* Invalidate the caches that both reset and travel must drop before changing
 * the staging view: perm caches, pinned dentries, and the cached shard dir
 * (the CLI is about to delete or reorganize the shard directories). Must be
 * called with sbi->staging.sem held for write. */
static void yolo_staging_quiesce(struct super_block *sb,
				 struct yolo_sb_info *sbi)
{
	struct dentry *old;

	atomic64_inc(&sbi->perm.gen);
	yolo_dentry_unpin_all(sb);

	/* shard_lock, not just staging.sem: creates reach the shard cache
	 * without taking staging.sem (see get_shard_dir). The epoch bump keeps
	 * an in-flight create from re-publishing its stale shard afterwards. */
	spin_lock(&sbi->staging.shard_lock);
	old = sbi->staging.shard_dentry;
	sbi->staging.shard_dentry = NULL;
	sbi->staging.shard_epoch++;
	spin_unlock(&sbi->staging.shard_lock);
	if (old)
		dput(old);
}

/* Replace the staged view. Caller holds staging.sem for write. */
static int yolo_set_view_locked(struct file *file, struct yolo_sb_info *sbi,
				u64 tree_ptr, u64 tree_len, u16 gen,
				u32 cow_ino_floor)
{
	struct super_block *sb = file_inode(file)->i_sb;
	int err;

	if (atomic_read(&sbi->staging.fd_count) > 0)
		return -EBUSY;

	yolo_staging_quiesce(sb, sbi);
	atomic_set(&sbi->staging.gen, gen);
	err = yolo_view_inject(file, sbi, tree_ptr, tree_len, gen,
			       cow_ino_floor);
	if (err) {
		/* Drop the partial view — a failed inject (e.g. base drift
		 * broke a redirect) falls back to the clean base, never a
		 * half-injected overlay. */
		yolo_staging_quiesce(sb, sbi);
	}
	return err;
}

static long yolo_restore_ioctl(struct file *file, unsigned long arg)
{
	struct yolo_sb_info *sbi = YOLO_SB(file_inode(file)->i_sb);
	struct yolo_ioc_restore hdr;
	int err;

	if (!sbi->staging.enabled)
		return -EOPNOTSUPP;
	if (copy_from_user(&hdr, (void __user *)arg, sizeof(hdr)))
		return -EFAULT;
	if (hdr.gen > U16_MAX || hdr.dirty > 1)
		return -EINVAL;
	/* A nonzero floor implies a marker, whose gen is >= 1. Accepting it with
	 * gen 0 would stamp snapshot-retained inos (<= floor) at gen 0 == current
	 * and let writes mutate them in place. */
	if (hdr.cow_ino_floor && !hdr.gen)
		return -EINVAL;

	down_write(&sbi->staging.sem);
	err = yolo_set_view_locked(file, sbi, hdr.tree_ptr, hdr.tree_len,
				   (u16)hdr.gen, hdr.cow_ino_floor);
	if (!err) {
		if ((u32)atomic_read(&sbi->staging.next_ino) < hdr.alloc_ino_floor)
			atomic_set(&sbi->staging.next_ino, (int)hdr.alloc_ino_floor);
		WRITE_ONCE(sbi->staging.dirty, hdr.dirty);
	}

	up_write(&sbi->staging.sem);
	return err;
}

static long yolo_travel_ioctl(struct file *file, unsigned long arg)
{
	struct super_block *sb = file_inode(file)->i_sb;
	struct yolo_sb_info *sbi = YOLO_SB(sb);
	struct yolo_ioc_travel hdr;
	u16 new_gen;
	int err = 0;

	if (!sbi->staging.enabled)
		return -EOPNOTSUPP;

	if (copy_from_user(&hdr, (void __user *)arg, sizeof(hdr)))
		return -EFAULT;

	/* gen 0 (the base) is a valid target: quiesce drops the overlay dentries
	 * and an empty inject adds none, so the mount falls through to the base —
	 * non-destructively (staged inodes + journal are kept, unlike RESET). */
	if (hdr.target_gen > U16_MAX)
		return -EINVAL;

	down_write(&sbi->staging.sem);
	/* Increment gen, inject entries, write T record */
	if (atomic_read(&sbi->staging.fd_count) > 0) {
		up_write(&sbi->staging.sem);
		return -EBUSY;
	}
	if (atomic_read(&sbi->staging.gen) >= U16_MAX) {
		up_write(&sbi->staging.sem);
		return -EOVERFLOW;
	}
	new_gen = (u16)(atomic_read(&sbi->staging.gen) + 1);

	/* floor 0: every store ino is > 0, so the whole injected view stamps
	 * at new_gen — travel keeps its write-in-place behavior. */
	err = yolo_set_view_locked(file, sbi, hdr.tree_ptr, hdr.tree_len,
				   new_gen, 0);
	if (!err)
		err = yolo_journal_travel(sbi, hdr.target_gen);
	/* Don't rollback gen on failure — a failed inject quiesces back to
	 * base, but inodes touched during injection may keep new_gen stamps
	 * in the icache; rolling gen back would make those read as current
	 * and skip COW.  Gen stays monotonic; the CLI can retry the
	 * operation or abort (which resets gen to 0). */
	if (!err)
		WRITE_ONCE(sbi->staging.dirty, false);

	up_write(&sbi->staging.sem);

	if (!err) {
		/* Best-effort: travel is already committed to the journal,
		 * so return success even if copy_to_user fails. */
		hdr.new_gen = new_gen;
		if (copy_to_user((void __user *)arg, &hdr, sizeof(hdr)))
			/* new_gen already in journal — userspace can recover */;
	}

	return err;
}

/* ── ioctl handler (all operations), dispatched from yolo_dir_fops ──── */

long yolo_ctl_ioctl(struct file *file, unsigned int cmd,
		    unsigned long arg)
{
	/* Gating-defeating ops must come from outside the mount: an agent (or
	 * the interactive shell) could otherwise grant itself access or answer
	 * its own ask prompts or install arbitrary path redirects.
	 * SNAPSHOT/RESOLVE are fine from inside. */
	switch (cmd) {
	case YOLO_IOC_RULE_SET:
	case YOLO_IOC_GET_ASK:
	case YOLO_IOC_PUT_DECISION:
	case YOLO_IOC_RESTORE:
	case YOLO_IOC_TRAVEL:
		if (yolo_caller_inside(file_inode(file)->i_sb))
			return -EPERM;
		break;
	default:
		break;
	}

	switch (cmd) {
	case YOLO_IOC_GET_ASK:
		return yolo_get_ask_ioctl(file, arg);

	case YOLO_IOC_PUT_DECISION:
		return yolo_put_decision_ioctl(file, arg);

	case YOLO_IOC_RULE_SET:
		return yolo_rule_set_ioctl(file, arg);

	case YOLO_IOC_RULE_RESOLVE:
		return yolo_rule_resolve_ioctl(file, arg);

	case YOLO_IOC_SNAPSHOT:
		return yolo_snapshot_ioctl(file, arg);

	case YOLO_IOC_TRAVEL:
		return yolo_travel_ioctl(file, arg);

	case YOLO_IOC_RESTORE:
		return yolo_restore_ioctl(file, arg);

	default:
		return -ENOTTY;
	}
}
