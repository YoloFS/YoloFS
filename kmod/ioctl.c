// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — control interface via .ctl control file.
 *
 * All ioctl operations go through the synthetic .ctl file at the mount
 * root.  The permission daemon claims exclusive daemon status on its
 * first GET_REQUEST call; only that fd may issue GET_REQUEST and
 * PUT_RESPONSE.  All other operations may be issued from any fd.
 *
 * On close of the daemon fd, any dispatched-but-unanswered requests
 * get the default decision.
 */

#include "agfs.h"
#include <linux/file.h>
#include <linux/vmalloc.h>
#include <asm/unaligned.h>

/* ── .ctl open/release ──────────────────────────────────────────────── */

static int agfs_ctl_open(struct inode *inode, struct file *file)
{
	return 0;
}

static int agfs_ctl_release(struct inode *inode, struct file *file)
{
	struct agfs_sb_info *sbi = AGFS_SB(inode->i_sb);

	if (file->private_data) {
		agfs_daemon_cleanup(sbi);
		atomic_set(&sbi->ask_engine.has_daemon, 0);
	}
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

	/* Claim daemon status on first call; reject if another fd already has it */
	if (!file->private_data) {
		if (atomic_cmpxchg(&eng->has_daemon, 0, 1))
			return -EBUSY;
		file->private_data = (void *)1;
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

	if (copy_from_user(&in, (void __user *)arg, sizeof(in)))
		return -EFAULT;

	if (in.decision > AGFS_PERM_HIDE)
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

/* ── Rule / mark ioctl handlers ─────────────────────────────────── */

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

	if (rule.perm > AGFS_PERM_HIDE) {
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

static long agfs_mark_ioctl(struct file *file, unsigned long arg)
{
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);
	struct agfs_ioc_mark mrk;
	char name_buf[AGFS_PATH_MAX];
	u16 gen;
	int err;

	if (!sbi->staging)
		return -EOPNOTSUPP;

	if (copy_from_user(&mrk, (void __user *)arg, sizeof(mrk)))
		return -EFAULT;

	err = agfs_copy_user_path(mrk.name_ptr, mrk.name_len, name_buf);
	if (err)
		return err;

	down_write(&sbi->staging_sem);
	if (atomic_read(&sbi->staging_fd_count) > 0) {
		up_write(&sbi->staging_sem);
		return -EBUSY;
	}
	if ((mrk.flags & AGFS_MARK_IF_CHANGED) && !READ_ONCE(sbi->dirty)) {
		up_write(&sbi->staging_sem);
		mrk.gen = 0;
		if (copy_to_user((void __user *)arg, &mrk, sizeof(mrk)))
			return -EFAULT;
		return 0;
	}
	if (atomic_read(&sbi->gen) >= U16_MAX) {
		up_write(&sbi->staging_sem);
		return -EOVERFLOW;
	}
	gen = (u16)atomic_inc_return(&sbi->gen);
	agfs_journal_mark(sbi, gen, name_buf);
	WRITE_ONCE(sbi->dirty, false);
	up_write(&sbi->staging_sem);

	/* Best-effort: mark is already committed to the journal,
	 * so return success even if copy_to_user fails. */
	mrk.gen = gen;
	if (copy_to_user((void __user *)arg, &mrk, sizeof(mrk)))
		/* gen already in journal — userspace can read it back */;

	return 0;
}

/* ── Jump ioctl handler ────────────────────────────────────────────── */

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
static int jump_inject_entry(struct tree_cursor *cur,
				struct agfs_sb_info *sbi,
				struct dentry *parent,
				const u8 *name_ptr, u16 name_len,
				u8 target, u16 gen)
{
	struct path lower_path;
	struct dentry *child;
	int err;

	switch (target) {
	case AGFS_TARGET_NONE: { /* tombstone */
		child = agfs_dentry_create(parent, (const char *)name_ptr,
					   name_len, AGFS_TARGET_NONE, NULL);
		return IS_ERR(child) ? PTR_ERR(child) : 0;
	}

	case AGFS_TARGET_INODE: { /* staged inode */
		u32 ino;

		err = read_le32(cur, &ino);
		if (err)
			return err;
		err = agfs_inode_path(sbi, ino, &lower_path);
		if (err)
			return err;
		child = agfs_dentry_create(parent, (const char *)name_ptr,
					   name_len, AGFS_TARGET_INODE,
					   &lower_path);
		if (IS_ERR(child))
			return PTR_ERR(child);
		AGFS_I(d_inode(child))->staging_gen = gen;
		return 0;
	}

	case AGFS_TARGET_PATH: { /* redirect, or passthrough if path_len == 0 */
		char path_buf[AGFS_PATH_MAX];
		const u8 *base_ptr;
		u16 base_len;

		err = read_le16(cur, &base_len);
		if (err)
			return err;

		if (base_len == 0) /* passthrough — no state to set */
			return 0;

		if (base_len >= AGFS_PATH_MAX)
			return -EINVAL;
		err = read_bytes(cur, base_len, &base_ptr);
		if (err)
			return err;

		memcpy(path_buf, base_ptr, base_len);
		path_buf[base_len] = '\0';
		err = kern_path(path_buf, LOOKUP_FOLLOW, &lower_path);
		if (err)
			return err;
		child = agfs_dentry_create(parent, (const char *)name_ptr,
					   name_len, AGFS_TARGET_PATH,
					   &lower_path);
		return IS_ERR(child) ? PTR_ERR(child) : 0;
	}

	default:
		return -EINVAL;
	}
}

static int agfs_jump_inject(struct file *file, struct agfs_sb_info *sbi,
			    struct agfs_ioc_jump *hdr, u16 gen)
{
	struct dir_frame stack[AGFS_JUMP_MAX_DEPTH];
	struct tree_cursor cur;
	u8 *kbuf;
	int depth;
	int err = 0;
	u16 root_count;

	if (hdr->tree_len > AGFS_JUMP_MAX_TREE_LEN)
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
		err = jump_inject_entry(&cur, sbi, stack[depth].dentry,
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

			if (depth + 1 >= AGFS_JUMP_MAX_DEPTH) {
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

static long agfs_jump_ioctl(struct file *file, unsigned long arg)
{
	struct super_block *sb = file_inode(file)->i_sb;
	struct agfs_sb_info *sbi = AGFS_SB(sb);
	struct agfs_ioc_jump hdr;
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
	agfs_dentry_unpin_all(sb);

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

	/* Invalidate shard cache before jump — CLI may reorganize inodes. */
	if (sbi->shard_dentry) {
		dput(sbi->shard_dentry);
		sbi->shard_dentry = NULL;
	}

	/* Jump mode: increment gen, inject entries, write J record */
	if (atomic_read(&sbi->gen) >= U16_MAX) {
		up_write(&sbi->staging_sem);
		return -EOVERFLOW;
	}
	new_gen = (u16)atomic_inc_return(&sbi->gen);

	err = agfs_jump_inject(file, sbi, &hdr, new_gen);
	if (!err)
		err = agfs_journal_jump(sbi, new_gen, hdr.target_gen);
	/* Don't rollback gen on failure — dirents may already be injected
	 * with new_gen.  Rolling back would leave those dirents with a gen
	 * higher than sbi->gen, breaking COW checks.  The CLI can retry
	 * the operation or abort (which resets gen to 0). */
	if (!err)
		WRITE_ONCE(sbi->dirty, false);

	up_write(&sbi->staging_sem);

	if (!err) {
		/* Best-effort: jump is already committed to the journal,
		 * so return success even if copy_to_user fails. */
		hdr.new_gen = new_gen;
		if (copy_to_user((void __user *)arg, &hdr, sizeof(hdr)))
			/* new_gen already in journal — userspace can recover */;
	}

	return err;
}

/* ── .ctl ioctl handler (all operations) ────────────────────────────── */

static long agfs_ctl_ioctl(struct file *file, unsigned int cmd,
			   unsigned long arg)
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

	case AGFS_IOC_MARK:
		return agfs_mark_ioctl(file, arg);

	case AGFS_IOC_JUMP:
		return agfs_jump_ioctl(file, arg);

	default:
		return -ENOTTY;
	}
}

const struct file_operations agfs_ctl_fops = {
	.open		= agfs_ctl_open,
	.release	= agfs_ctl_release,
	.unlocked_ioctl	= agfs_ctl_ioctl,
	.compat_ioctl	= agfs_ctl_ioctl,
};
