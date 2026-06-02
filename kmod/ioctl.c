// SPDX-License-Identifier: GPL-2.0
/*
 * yolofs — control interface (ioctls on the mount-root directory).
 *
 * All control operations are ioctls on a directory fd in the mount (the CLI
 * opens the mount root; commands run inside the sandbox open "."). Only the
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
#include <asm/unaligned.h>

/* True if the caller is chrooted into this mount (an agent command or the
 * interactive `yolo` shell), as opposed to a normal terminal outside it.
 * Gating-defeating ops are refused from inside so nothing in the sandbox can
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

/* ── Daemon release ──────────────────────────────────────────────────
 * Called from yolo_dir_release when a mount-root directory fd closes. Cleans
 * up only if this fd was the connected daemon — tracked by file identity, not
 * private_data (dir fds use private_data for the readdir cursor).
 */
void yolo_ctl_release(struct file *file)
{
	struct yolo_sb_info *sbi = YOLO_SB(file_inode(file)->i_sb);
	struct yolo_ask_engine *eng = &sbi->ask_engine;

	if (cmpxchg(&eng->daemon_file, file, NULL) == file) {
		yolo_daemon_cleanup(sbi);
		atomic_set(&eng->has_daemon, 0);
	}
}

/* ── GET_ASK: dequeue pending request ──────────────────────────── */

static long yolo_get_ask_ioctl(struct file *file, unsigned long arg)
{
	struct yolo_sb_info *sbi = YOLO_SB(file_inode(file)->i_sb);
	struct yolo_ask_engine *eng = &sbi->ask_engine;
	struct yolo_perm_request *req;
	struct yolo_ioc_ask out;
	int err;
	__u16 path_len;

	/* Claim daemon status on first call; reject if another fd already has it.
	 * Tracked by file identity (private_data holds the dir's readdir cursor). */
	if (eng->daemon_file != file) {
		if (cmpxchg(&eng->daemon_file, NULL, file) != NULL)
			return -EBUSY;
		atomic_set(&eng->has_daemon, 1);
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
			       struct yolo_perm_request, list);
	list_del_init(&req->list);
	kref_get(&req->ref); /* daemon takes a reference */
	spin_unlock(&eng->pending_lock);

	path_len = req->path_len;

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
	list_add_tail(&req->list, &eng->pending_reqs);
	spin_unlock(&eng->pending_lock);
	wake_up_interruptible(&eng->request_waitq);
	kref_put(&req->ref, yolo_perm_request_release);
	return err;
}

/* ── PUT_DECISION: submit decision ─────────────────────────────────── */

static long yolo_put_decision_ioctl(struct file *file, unsigned long arg)
{
	struct yolo_ask_engine *eng = &YOLO_SB(file_inode(file)->i_sb)->ask_engine;
	struct yolo_ioc_decision in;
	struct yolo_perm_request *req, *tmp;
	bool found = false;

	if (copy_from_user(&in, (void __user *)arg, sizeof(in)))
		return -EFAULT;

	if (in.decision > YOLO_PERM_HIDE)
		return -EINVAL;

	spin_lock(&eng->dispatch_lock);
	list_for_each_entry_safe(req, tmp, &eng->dispatched, list) {
		if (req->id == in.id) {
			req->decision = (enum yolo_perm)in.decision;
			list_del_init(&req->list);
			found = true;
			break;
		}
	}
	spin_unlock(&eng->dispatch_lock);

	if (!found)
		return -ENOENT;

	complete(&req->done);
	kref_put(&req->ref, yolo_perm_request_release);
	return 0;
}

/* ── Cleanup dispatched requests on daemon fd close ────────────────── */

void yolo_daemon_cleanup(struct yolo_sb_info *sbi)
{
	struct yolo_ask_engine *eng = &sbi->ask_engine;
	struct yolo_perm_request *req, *tmp;

	spin_lock(&eng->dispatch_lock);
	list_for_each_entry_safe(req, tmp, &eng->dispatched, list) {
		req->decision = eng->default_perm;
		list_del_init(&req->list);
		complete(&req->done);
		kref_put(&req->ref, yolo_perm_request_release);
	}
	spin_unlock(&eng->dispatch_lock);
}

/* ── Release all rule-pinned dentries ───────────────────────────────── */

void yolo_release_pinned_rules(struct yolo_sb_info *sbi)
{
	LIST_HEAD(local);
	struct yolo_dentry_info *di, *tmp;

	spin_lock(&sbi->pinned_rules_lock);
	list_splice_init(&sbi->pinned_rules, &local);
	spin_unlock(&sbi->pinned_rules_lock);

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

/* ── Path copy helper ──────────────────────────────────────────────── */

/*
 * Copy a variable-length path from userspace into a caller-provided buffer.
 * The buffer must be at least YOLO_PATH_MAX bytes.
 * Paths are limited to YOLO_PATH_MAX-1 bytes (same as internal buffers).
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

static int yolo_resolve_rule(struct file *file, unsigned long arg,
			     struct yolo_ioc_rule *rule,
			     struct path *rule_path,
			     struct yolo_dentry_info **di_out)
{
	char path_buf[YOLO_PATH_MAX];
	int err;

	if (copy_from_user(rule, (void __user *)arg, sizeof(*rule)))
		return -EFAULT;

	err = yolo_copy_user_path(rule->path_ptr, rule->path_len, path_buf);
	if (err)
		return err;

	err = kern_path(path_buf, LOOKUP_FOLLOW, rule_path);
	if (err)
		return err;

	if (rule_path->dentry->d_sb != file_inode(file)->i_sb) {
		path_put(rule_path);
		return -EXDEV;
	}

	*di_out = YOLO_D(rule_path->dentry);
	if (!*di_out) {
		path_put(rule_path);
		return -ENOENT;
	}

	return 0;
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
			spin_lock(&sbi->pinned_rules_lock);
			if (!list_empty(&di->rule_pin))
				list_del_init(&di->rule_pin);
			spin_unlock(&sbi->pinned_rules_lock);
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
			spin_lock(&sbi->pinned_rules_lock);
			if (list_empty(&di->rule_pin))
				list_add(&di->rule_pin, &sbi->pinned_rules);
			spin_unlock(&sbi->pinned_rules_lock);
		}
	}

	atomic64_inc(&sbi->perm_gen);
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

	rule.perm = (__u8)yolo_resolve_perm(rule_path.dentry);
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

	if (!sbi->staging)
		return -EOPNOTSUPP;

	if (copy_from_user(&snap, (void __user *)arg, sizeof(snap)))
		return -EFAULT;

	err = yolo_copy_user_path(snap.name_ptr, snap.name_len, name_buf);
	if (err)
		return err;

	down_write(&sbi->staging_sem);
	if (atomic_read(&sbi->staging_fd_count) > 0) {
		up_write(&sbi->staging_sem);
		return -EBUSY;
	}
	if ((snap.flags & YOLO_SNAPSHOT_IF_CHANGED) && !READ_ONCE(sbi->dirty)) {
		up_write(&sbi->staging_sem);
		snap.gen = 0;
		if (copy_to_user((void __user *)arg, &snap, sizeof(snap)))
			return -EFAULT;
		return 0;
	}
	if (atomic_read(&sbi->gen) >= U16_MAX) {
		up_write(&sbi->staging_sem);
		return -EOVERFLOW;
	}
	gen = (u16)atomic_inc_return(&sbi->gen);
	yolo_journal_snapshot(sbi, gen, name_buf);
	WRITE_ONCE(sbi->dirty, false);
	up_write(&sbi->staging_sem);

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
	*out = get_unaligned_le16(c->buf);
	c->buf += 2;
	return 0;
}

static inline int read_le32(struct tree_cursor *c, u32 *out)
{
	if (c->buf + 4 > c->end)
		return -EINVAL;
	*out = get_unaligned_le32(c->buf);
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
 * dentry under @parent.  Scaffold (tag 0) entries have no payload.
 */
static int travel_inject_entry(struct tree_cursor *cur,
				struct yolo_sb_info *sbi,
				struct dentry *parent,
				const u8 *name_ptr, u16 name_len,
				u8 target, u16 gen)
{
	struct path lower_path;
	struct dentry *child;
	int err;

	switch (target) {
	case YOLO_TARGET_NONE: { /* tombstone */
		child = yolo_dentry_create(parent, (const char *)name_ptr,
					   name_len, YOLO_TARGET_NONE, NULL);
		return IS_ERR(child) ? PTR_ERR(child) : 0;
	}

	case YOLO_TARGET_INODE: { /* staged inode */
		u32 ino;

		err = read_le32(cur, &ino);
		if (err)
			return err;
		err = yolo_inode_path(sbi, ino, &lower_path);
		if (err)
			return err;
		child = yolo_dentry_create(parent, (const char *)name_ptr,
					   name_len, YOLO_TARGET_INODE,
					   &lower_path);
		if (IS_ERR(child))
			return PTR_ERR(child);
		YOLO_I(d_inode(child))->staging_gen = gen;
		return 0;
	}

	case YOLO_TARGET_PATH: { /* redirect, or passthrough if path_len == 0 */
		char path_buf[YOLO_PATH_MAX];
		const u8 *base_ptr;
		u16 base_len;

		err = read_le16(cur, &base_len);
		if (err)
			return err;

		if (base_len == 0) /* passthrough — no state to set */
			return 0;

		if (base_len >= YOLO_PATH_MAX)
			return -EINVAL;
		err = read_bytes(cur, base_len, &base_ptr);
		if (err)
			return err;

		memcpy(path_buf, base_ptr, base_len);
		path_buf[base_len] = '\0';
		err = kern_path(path_buf, LOOKUP_FOLLOW, &lower_path);
		if (err)
			return err;
		child = yolo_dentry_create(parent, (const char *)name_ptr,
					   name_len, YOLO_TARGET_PATH,
					   &lower_path);
		return IS_ERR(child) ? PTR_ERR(child) : 0;
	}

	default:
		return -EINVAL;
	}
}

static int yolo_travel_inject(struct file *file, struct yolo_sb_info *sbi,
			    struct yolo_ioc_travel *hdr, u16 gen)
{
	struct dir_frame stack[YOLO_TRAVEL_MAX_DEPTH];
	struct tree_cursor cur;
	u8 *kbuf;
	int depth;
	int err = 0;
	u16 root_count;

	if (hdr->tree_len > YOLO_TRAVEL_MAX_TREE_LEN)
		return -EINVAL;

	kbuf = vmalloc(hdr->tree_len);
	if (!kbuf)
		return -ENOMEM;

	if (copy_from_user(kbuf, (const void __user *)hdr->tree_ptr,
			   hdr->tree_len)) {
		vfree(kbuf);
		return -EFAULT;
	}

	cur.buf = kbuf;
	cur.end = kbuf + hdr->tree_len;

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
		err = travel_inject_entry(&cur, sbi, stack[depth].dentry,
					   name_ptr, name_len, target, gen);
		if (err)
			goto out_unwind;

		/* Read child_count */
		err = read_le16(&cur, &child_count);
		if (err)
			goto out_unwind;

		/* Skip nodes with no children to descend into. */
		if (child_count == 0)
			continue;

		{
			struct dentry *child;

			if (depth + 1 >= YOLO_TRAVEL_MAX_DEPTH) {
				err = -EINVAL;
				goto out_unwind;
			}
			child = lookup_one_len_unlocked(
					(const char *)name_ptr,
					stack[depth].dentry, name_len);
			if (IS_ERR(child)) {
				err = PTR_ERR(child);
				goto out_unwind;
			}
			depth++;
			stack[depth].dentry = child;
			stack[depth].remaining = child_count;
		}
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

static long yolo_travel_ioctl(struct file *file, unsigned long arg)
{
	struct super_block *sb = file_inode(file)->i_sb;
	struct yolo_sb_info *sbi = YOLO_SB(sb);
	struct yolo_ioc_travel hdr;
	u16 new_gen;
	int err = 0;

	if (!sbi->staging)
		return -EOPNOTSUPP;

	if (copy_from_user(&hdr, (void __user *)arg, sizeof(hdr)))
		return -EFAULT;

	if (hdr.target_gen > U16_MAX)
		return -EINVAL;

	down_write(&sbi->staging_sem);
	if (atomic_read(&sbi->staging_fd_count) > 0) {
		up_write(&sbi->staging_sem);
		return -EBUSY;
	}

	/* Wipe perm caches, pinned dentries, dentry cache */
	atomic64_inc(&sbi->perm_gen);
	yolo_dentry_unpin_all(sb);

	if (hdr.target_gen == 0) {
		/* Reset mode (commit/abort): no entries, no journal write */
		atomic_set(&sbi->gen, 0);
		WRITE_ONCE(sbi->dirty, false);
		/* Invalidate shard cache — CLI is about to delete shard dirs. */
		if (sbi->shard_dentry) {
			dput(sbi->shard_dentry);
			sbi->shard_dentry = NULL;
		}
		up_write(&sbi->staging_sem);
		return 0;
	}

	/* Invalidate shard cache before travel — CLI may reorganize inodes. */
	if (sbi->shard_dentry) {
		dput(sbi->shard_dentry);
		sbi->shard_dentry = NULL;
	}

	/* Travel mode: increment gen, inject entries, write T record */
	if (atomic_read(&sbi->gen) >= U16_MAX) {
		up_write(&sbi->staging_sem);
		return -EOVERFLOW;
	}
	new_gen = (u16)atomic_inc_return(&sbi->gen);

	err = yolo_travel_inject(file, sbi, &hdr, new_gen);
	if (!err)
		err = yolo_journal_travel(sbi, new_gen, hdr.target_gen);
	/* Don't rollback gen on failure — dirents may already be injected
	 * with new_gen.  Rolling back would leave those dirents with a gen
	 * higher than sbi->gen, breaking COW checks.  The CLI can retry
	 * the operation or abort (which resets gen to 0). */
	if (!err)
		WRITE_ONCE(sbi->dirty, false);

	up_write(&sbi->staging_sem);

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
	/* Gating-defeating ops must come from outside the sandbox: an agent (or
	 * the interactive shell) could otherwise grant itself access or answer
	 * its own ask prompts. SNAPSHOT/TRAVEL/RESOLVE are fine from inside. */
	switch (cmd) {
	case YOLO_IOC_RULE_SET:
	case YOLO_IOC_GET_ASK:
	case YOLO_IOC_PUT_DECISION:
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

	default:
		return -ENOTTY;
	}
}
