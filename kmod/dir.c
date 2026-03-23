// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — directory file operations.
 *
 * dir_open, dir_release, readdir (merged iterate_shared).
 */

#include "agfs.h"
#include <linux/file.h>

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
	file->private_data = di;
	return 0;
}

/* ── dir_release ───────────────────────────────────────────────────── */

static int agfs_dir_release(struct inode *inode, struct file *file)
{
	struct agfs_dir_info *di = AGFS_DI(file);
	struct agfs_sb_info *sbi = AGFS_SB(inode->i_sb);

	if (di) {
		if (file == READ_ONCE(sbi->ask_engine.daemon_file))
			agfs_daemon_cleanup(sbi);

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
		bool overridden =
			!agfs_dstate_is_passthrough(AGFS_D(child)->dstate);
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
static bool agfs_emit_dirents(struct dentry *parent, struct dir_context *ctx,
			      loff_t *off)
{
	struct dentry *child;

	spin_lock(&parent->d_lock);
	hlist_for_each_entry(child, &parent->d_children, d_sib) {
		struct agfs_dentry_info *di = AGFS_D(child);

		if (!di || agfs_dstate_is_passthrough(di->dstate))
			continue;

		if (agfs_dstate_is_tombstone(di->dstate))
			continue;

		dget_dlock(child);
		spin_unlock(&parent->d_lock);

		if (*off < ctx->pos) {
			(*off)++;
		} else if (!dir_emit(ctx, child->d_name.name,
				     child->d_name.len,
				     agfs_dstate_emit_ino(di->dstate),
				     agfs_dstate_d_type(di->dstate))) {
			dput(child);
			return true;
		} else {
			(*off)++;
			ctx->pos++;
		}

		dput(child);
		spin_lock(&parent->d_lock);
	}
	spin_unlock(&parent->d_lock);
	return false;
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
	 * so the skip logic in agfs_fill_base is a no-op (the lower file
	 * already resumes at the right position).
	 */
	if (di->dirent_off && ctx->pos >= di->dirent_off) {
		off = ctx->pos;
	} else {
		/* Phase 1: emit non-deleted dirent entries */
		if (agfs_emit_dirents(file_dentry(file), ctx, &off))
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
	.unlocked_ioctl	= agfs_ioctl,
	.compat_ioctl	= agfs_ioctl,
};
