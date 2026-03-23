// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — directory file operations.
 *
 * dir_open, dir_release, readdir (merged iterate_shared).
 */

#include "agfs.h"
#include <linux/file.h>
#include <linux/dcache.h>

extern struct dentry *file_dentry(const struct file *file);

static struct dentry *agfs_alloc_cursor(struct dentry *parent)
{
	struct dentry *cursor = d_alloc_anon(parent->d_sb);

	if (!cursor)
		return NULL;
	cursor->d_flags |= DCACHE_DENTRY_CURSOR;
	cursor->d_parent = dget(parent);
	return cursor;
}

/* ── dir_open ──────────────────────────────────────────────────────── */

static int agfs_dir_open(struct inode *inode, struct file *file)
{
	struct agfs_dir_info *di;
	struct file *lower_file;
	struct path lower_path;

	di = kzalloc(sizeof(*di), GFP_KERNEL);
	if (!di)
		return -ENOMEM;

	agfs_get_lower_path(file->f_path.dentry, &lower_path);
	lower_file = dentry_open(&lower_path, file->f_flags, current_cred());
	agfs_put_lower_path(file->f_path.dentry, &lower_path);
	if (IS_ERR(lower_file)) {
		kfree(di);
		return PTR_ERR(lower_file);
	}

	di->fi.lower_file = lower_file;
	di->phase1_cursor = agfs_alloc_cursor(file_dentry(file));
	if (!di->phase1_cursor) {
		fput(lower_file);
		kfree(di);
		return -ENOMEM;
	}
	file->private_data = di;
	return 0;
}

/* ── dir_release ───────────────────────────────────────────────────── */

static int agfs_dir_release(struct inode *inode, struct file *file)
{
	struct agfs_dir_info *di = AGFS_DI(file);

	if (di) {
		if (di->phase1_cursor) {
			dput(di->phase1_cursor);
			di->phase1_cursor = NULL;
		}
		if (di->fi.lower_file)
			fput(di->fi.lower_file);
		kfree(di);
		file->private_data = NULL;
	}
	return 0;
}

/* ── Directory: readdir / iterate_shared (merged listing) ───────────── */

/*
 * Merged readdir: dirents first, then base entries not overridden.
 *
 * The VFS holds inode_lock_shared(dir) for the duration of iterate_shared,
 * so the dirent table is stable — no checkpoint or temporary hash set needed.
 * We iterate the dirent table directly for emission and dedup.
 */

/* ── filldir callback for base directory reading ───────────────────── */

struct agfs_readdir_data {
	struct dir_context	ctx;
	struct dentry		*dentry;	/* parent dentry for d_lookup */
	struct dir_context	*caller_ctx;
	loff_t			*off;
};

static bool agfs_fill_base(struct dir_context *ctx, const char *name,
			   int namelen, loff_t offset, u64 ino,
			   unsigned int d_type)
{
	struct agfs_readdir_data *rdd =
		container_of(ctx, struct agfs_readdir_data, ctx);
	struct qstr qname = QSTR_INIT(name, namelen);
	struct dentry *child;

	/* Check if this base entry is overridden by a staged entry */
	qname.hash = full_name_hash(rdd->dentry, name, namelen);
	child = d_lookup(rdd->dentry, &qname);
	if (child) {
		bool overridden = AGFS_D(child)->kind != AGFS_DKIND_UNSET;
		dput(child);
		if (overridden)
			return true; /* skip — overridden */
	}

	if (*rdd->off < rdd->caller_ctx->pos) {
		(*rdd->off)++;
		return true;
	}
	if (!dir_emit(rdd->caller_ctx, name, namelen, ino, d_type))
		return false;
	(*rdd->off)++;
	rdd->caller_ctx->pos++;
	return true;
}

/*
 * Emit non-deleted staged entries from parent's d_children.
 * Caller holds inode_lock_shared(dir) via VFS iterate_shared.
 * Returns true if the dir_emit buffer filled up.
 */
static struct dentry *agfs_next_staged_child(struct dentry *parent,
					     struct hlist_node **p,
					     struct dentry *last)
{
	struct dentry *child;

	spin_lock(&parent->d_lock);
	while (*p) {
		child = hlist_entry(*p, struct dentry, d_sib);
		*p = child->d_sib.next;
		if (child->d_flags & DCACHE_DENTRY_CURSOR)
			continue;
		if (AGFS_D(child)->kind == AGFS_DKIND_UNSET)
			continue;
		if (AGFS_D(child)->kind == AGFS_DKIND_TOMBSTONE)
			continue;
		spin_lock_nested(&child->d_lock, DENTRY_D_LOCK_NESTED);
		if (AGFS_D(child)->kind == AGFS_DKIND_UNSET ||
		    AGFS_D(child)->kind == AGFS_DKIND_TOMBSTONE) {
			spin_unlock(&child->d_lock);
			continue;
		}
		dget_dlock(child);
		spin_unlock(&child->d_lock);
		spin_unlock(&parent->d_lock);
		dput(last);
		return child;
	}
	spin_unlock(&parent->d_lock);
	dput(last);
	return NULL;
}

static bool agfs_emit_dirents(struct dentry *parent, struct dir_context *ctx,
			      loff_t *off, struct agfs_dir_info *di)
{
	struct dentry *cursor = di->phase1_cursor;
	struct dentry *next = NULL;
	struct hlist_node *p;

	if (!cursor)
		return false;

	if (ctx->pos <= 2)
		p = parent->d_children.first;
	else
		p = cursor->d_sib.next;

	*off = ctx->pos;
	while ((next = agfs_next_staged_child(parent, &p, next)) != NULL) {
		if (!dir_emit(ctx, next->d_name.name,
			      next->d_name.len,
			      d_inode(next)->i_ino,
			      fs_umode_to_dtype(d_inode(next)->i_mode)))
			break;
		ctx->pos++;
		(*off)++;
		p = next->d_sib.next;
	}

	spin_lock(&parent->d_lock);
	hlist_del_init(&cursor->d_sib);
	if (next)
		hlist_add_before(&cursor->d_sib, &next->d_sib);
	spin_unlock(&parent->d_lock);

	dput(next);

	return next != NULL;
}

/* ── readdir entry point ───────────────────────────────────────────── */

static int agfs_readdir(struct file *file, struct dir_context *ctx)
{
	struct agfs_dir_info *di = AGFS_DI(file);
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);
	struct file *lower_file = di->fi.lower_file;
	struct agfs_readdir_data rdd;
	loff_t off = 0;
	int err = 0;

	if (!lower_file)
		return -EIO;

	/* No staging → unset */
	if (!sbi->staging || !sbi->inodes_dir.dentry) {
		lower_file->f_pos = ctx->pos;
		err = iterate_dir(lower_file, ctx);
		file->f_pos = lower_file->f_pos;
		return err;
	}

	/*
	 * Merge path: phase 1 (dirents) then phase 2 (base).
	 *
	 * If ctx->pos >= di->dirent_off we have already emitted all
	 * dirents in a previous getdents64 call — skip phase 1 and
	 * resume phase 2 from the saved lower f_pos.  Set off = ctx->pos
	 * so the skip logic in agfs_fill_base is a no-op (the lower file
	 * already resumes at the right position).
	 */
	if (di->dirent_off && ctx->pos >= di->dirent_off) {
		off = ctx->pos;
	} else {
		/* Phase 1: emit non-deleted dirent entries */
		if (agfs_emit_dirents(file_dentry(file), ctx, &off, di))
			return 0;
		di->dirent_off = off;
		/*
		 * If no staged entries were emitted (off == 0), sync off
		 * with the caller's position so the skip logic in
		 * agfs_fill_base does not double-skip base entries on
		 * resumed getdents64 calls.
		 */
		if (!off)
			off = ctx->pos;
	}

	/* Phase 2: read base directory, skip overridden names */
	rdd.ctx.actor = agfs_fill_base;
	rdd.ctx.pos = 0;
	rdd.dentry = file_dentry(file);
	rdd.caller_ctx = ctx;
	rdd.off = &off;

	lower_file->f_pos = di->base_pos;
	err = iterate_dir(lower_file, &rdd.ctx);
	di->base_pos = lower_file->f_pos;

	return err;
}

/* ── Dir Ops Table ─────────────────────────────────────────────────── */

const struct file_operations agfs_dir_fops = {
	.open		= agfs_dir_open,
	.release	= agfs_dir_release,
	.iterate_shared	= agfs_readdir,
	.llseek		= no_llseek,
	.fsync		= noop_fsync,
};
