// SPDX-License-Identifier: GPL-2.0
/*
 * yolofs — directory file operations.
 *
 * dir_open, dir_release, readdir (merged iterate_shared).
 */

#include "yolofs.h"
#include <linux/file.h>
#include <linux/dcache.h>

extern struct dentry *file_dentry(const struct file *file);

static struct dentry *yolo_alloc_cursor(struct dentry *parent)
{
	struct dentry *cursor = d_alloc_anon(parent->d_sb);

	if (!cursor)
		return NULL;
	cursor->d_flags |= DCACHE_DENTRY_CURSOR;
	cursor->d_parent = dget(parent);
	return cursor;
}

/* ── dir_open ──────────────────────────────────────────────────────── */

static int yolo_dir_open(struct inode *inode, struct file *file)
{
	struct yolo_sb_info *sbi = YOLO_SB(inode->i_sb);
	struct yolo_dir_info *di;
	struct file *lower_file;
	struct path lower_path;

	/* Hidden directories are invisible — return ENOENT.
	 * All other perms (including deny) allow readdir. */
	if (sbi->permission) {
		struct dentry *dentry = file->f_path.dentry;
		struct yolo_inode_info *ii = YOLO_I(d_inode(dentry));
		if (ii->perm_gen != atomic64_read(&sbi->perm_gen))
			yolo_cache_perm(d_inode(dentry), dentry);
		if (ii->cached_perm == YOLO_PERM_HIDE)
			return -ENOENT;
	}

	di = kzalloc(sizeof(*di), GFP_KERNEL);
	if (!di)
		return -ENOMEM;

	yolo_get_lower_path(file->f_path.dentry, &lower_path);
	lower_file = dentry_open(&lower_path, file->f_flags, current_cred());
	yolo_put_lower_path(file->f_path.dentry, &lower_path);
	if (IS_ERR(lower_file)) {
		kfree(di);
		return PTR_ERR(lower_file);
	}

	di->fi.lower_file = lower_file;
	di->phase1_cursor = yolo_alloc_cursor(file_dentry(file));
	if (!di->phase1_cursor) {
		fput(lower_file);
		kfree(di);
		return -ENOMEM;
	}
	file->private_data = di;
	return 0;
}

/* ── dir_release ───────────────────────────────────────────────────── */

static int yolo_dir_release(struct inode *inode, struct file *file)
{
	struct yolo_dir_info *di = YOLO_DI(file);

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
 * so the dirent table is stable — no snapshot or temporary hash set needed.
 * We iterate the dirent table directly for emission and dedup.
 */

/* ── filldir callback for base directory reading ───────────────────── */

struct yolo_readdir_data {
	struct dir_context	ctx;
	struct dentry		*dentry;	/* parent dentry for d_lookup */
	struct dir_context	*caller_ctx;
	loff_t			*off;
};

static bool yolo_base_entry_overridden(struct dentry *parent,
				       const char *name, int namelen)
{
	struct qstr qname = QSTR_INIT(name, namelen);
	struct dentry *child;
	bool overridden;

	qname.hash = full_name_hash(parent, name, namelen);
	child = d_lookup(parent, &qname);
	if (!child)
		return false;

	overridden = YOLO_D(child)->pinned;
	dput(child);
	return overridden;
}

static bool yolo_fill_base(struct dir_context *ctx, const char *name,
			   int namelen, loff_t offset, u64 ino,
			   unsigned int d_type)
{
	struct yolo_readdir_data *rdd =
		container_of(ctx, struct yolo_readdir_data, ctx);

	/* Check if this base entry is overridden by a pinned entry */
	if (yolo_base_entry_overridden(rdd->dentry, name, namelen))
		return true; /* skip — overridden */

	/* Check if this entry is hidden by permission rules.
	 * Scan the parent's pinned rule dentries for a matching name
	 * with YOLO_PERM_HIDE. Rule dentries are pinned (dget'd) so
	 * they're always in the dcache as children of the parent. */
	if (YOLO_SB(rdd->dentry->d_sb)->permission) {
		struct dentry *child;
		bool is_hidden = false;

		spin_lock(&rdd->dentry->d_lock);
		hlist_for_each_entry(child, &rdd->dentry->d_children, d_sib) {
			struct yolo_dentry_info *cdi = YOLO_D(child);
			if (cdi && cdi->perm == YOLO_PERM_HIDE &&
			    child->d_name.len == namelen &&
			    !memcmp(child->d_name.name, name, namelen)) {
				is_hidden = true;
				break;
			}
		}
		spin_unlock(&rdd->dentry->d_lock);

		if (is_hidden)
			return true; /* skip — hidden */
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
static bool yolo_should_emit_staged_child(const struct dentry *child)
{
	/*
	 * Tombstones (NONE + pinned) stay pinned in dcache so lookup/readdir
	 * can hide the base name, but phase 1 must not emit them as dirents.
	 */
	return YOLO_D(child)->pinned &&
	       !(YOLO_D(child)->target == YOLO_TARGET_NONE);
}

static struct dentry *yolo_next_staged_child(struct dentry *parent,
					     struct hlist_node **p,
					     struct dentry *last)
{
	struct dentry *child;

	/*
	 * inode_lock_shared(dir) keeps ordinary directory mutations serialized
	 * against iterate_shared, but d_children traversal and cursor links are
	 * still dcache state and must be protected by parent->d_lock.
	 */
	spin_lock(&parent->d_lock);
	while (*p) {
		child = hlist_entry(*p, struct dentry, d_sib);
		*p = child->d_sib.next;
		if (child->d_flags & DCACHE_DENTRY_CURSOR)
			continue;
		/*
		 * Fast pre-check without child->d_lock: skip children that do
		 * not contribute a visible phase-1 dirent. Re-checked below
		 * under d_lock to close the TOCTOU window.
		 */
		if (!yolo_should_emit_staged_child(child))
			continue;
		/*
		 * Take a stable ref before dropping parent->d_lock.  child->d_lock
		 * is needed for dget_dlock(), and the staged-state check is
		 * repeated while we hold it so out-of-band resets cannot hand back
		 * a child that stopped being visible between the pre-check and ref.
		 */
		spin_lock_nested(&child->d_lock, DENTRY_D_LOCK_NESTED);
		if (!yolo_should_emit_staged_child(child)) {
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

static void yolo_readdir_update_cursor(struct dentry *parent,
				       struct dentry *cursor,
				       struct dentry *next)
{
	spin_lock(&parent->d_lock);
	hlist_del_init(&cursor->d_sib);
	if (next)
		hlist_add_before(&cursor->d_sib, &next->d_sib);
	spin_unlock(&parent->d_lock);
}

static bool yolo_emit_dirents(struct dentry *parent, struct dir_context *ctx,
			      loff_t *off, struct yolo_dir_info *di)
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
	while ((next = yolo_next_staged_child(parent, &p, next)) != NULL) {
		if (!dir_emit(ctx, next->d_name.name,
			      next->d_name.len,
			      d_inode(next)->i_ino,
			      fs_umode_to_dtype(d_inode(next)->i_mode)))
			break;
		ctx->pos++;
		(*off)++;
		p = next->d_sib.next;
	}

	/* parent->d_lock protects the saved cursor's position in d_children. */
	yolo_readdir_update_cursor(parent, cursor, next);

	dput(next);

	return next != NULL;
}

/* ── readdir entry point ───────────────────────────────────────────── */

static int yolo_readdir(struct file *file, struct dir_context *ctx)
{
	struct yolo_dir_info *di = YOLO_DI(file);
	struct yolo_sb_info *sbi = YOLO_SB(file_inode(file)->i_sb);
	struct file *lower_file = di->fi.lower_file;
	struct yolo_readdir_data rdd;
	loff_t off = 0;
	int err = 0;

	if (!lower_file)
		return -EIO;

	/* No staging → passthrough */
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
	 * so the skip logic in yolo_fill_base is a no-op (the lower file
	 * already resumes at the right position).
	 */
	if (di->dirent_off && ctx->pos >= di->dirent_off) {
		off = ctx->pos;
	} else {
		/* Phase 1: emit non-deleted dirent entries */
		if (yolo_emit_dirents(file_dentry(file), ctx, &off, di))
			return 0;
		di->dirent_off = off;
		/*
		 * If no staged entries were emitted (off == 0), sync off
		 * with the caller's position so the skip logic in
		 * yolo_fill_base does not double-skip base entries on
		 * resumed getdents64 calls.
		 */
		if (!off)
			off = ctx->pos;
	}

	/* Phase 2: read base directory, skip overridden names */
	rdd.ctx.actor = yolo_fill_base;
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

const struct file_operations yolo_dir_fops = {
	.open		= yolo_dir_open,
	.release	= yolo_dir_release,
	.iterate_shared	= yolo_readdir,
	.llseek		= no_llseek,
	.fsync		= noop_fsync,
};
