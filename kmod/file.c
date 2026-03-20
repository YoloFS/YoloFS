// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — file operations.
 *
 * open (perm gating + staging redirect), read_iter, write_iter,
 * mmap, fsync, release, llseek, readdir.
 */

#include "agfs.h"
#include <linux/file.h>
#include <linux/mm.h>

/* ── open helpers ───────────────────────────────────────────────────── */

static struct file *agfs_open_lower(struct dentry *dentry, int flags)
{
	struct path lower_path;
	struct file *f;

	agfs_get_lower_path(dentry, &lower_path);
	f = dentry_open(&lower_path, flags, current_cred());
	agfs_put_lower_path(dentry, &lower_path);
	return f;
}

static int agfs_check_open_perm(struct agfs_sb_info *sbi,
				struct dentry *dentry,
				struct file *file, char *buf)
{
	struct agfs_inode_info *ii = AGFS_I(d_inode(dentry));
	enum agfs_perm perm = ii->cached_perm;
	int err;

	if (ii->perm_gen != atomic64_read(&sbi->perm_gen)) {
		perm = agfs_resolve_perm(dentry);
		ii->cached_perm = perm;
		ii->perm_gen = atomic64_read(&sbi->perm_gen);
	}

	if (perm == AGFS_PERM_ASK) {
		unsigned int op;

		if (file->f_mode & FMODE_EXEC)
			op = AGFS_OP_EXEC;
		else if (file->f_flags & (O_WRONLY | O_RDWR | O_APPEND | O_TRUNC))
			op = AGFS_OP_WRITE;
		else
			op = AGFS_OP_READ;

		err = agfs_dentry_relpath(dentry, buf, AGFS_PATH_MAX);
		if (err)
			return err;
		err = agfs_ask_userspace(sbi, dentry, buf, op, &perm);
		if (err)
			return err;
	}

	return agfs_check_perm(perm, file->f_flags);
}

/*
 * Open a staged inode by ino, incrementing staging_fd_count.
 * On error, decrements the count and returns ERR_PTR.
 *
 * dentry_open() does not apply O_TRUNC (that is normally done by the
 * VFS after f_op->open returns), and agfs_setattr intentionally strips
 * ATTR_SIZE for staged files.  So we must truncate the lower inode
 * ourselves before opening.
 */
static struct file *agfs_open_staged_ino(struct agfs_sb_info *sbi,
					 u64 ino, int flags)
{
	struct path ino_p;
	struct file *f;
	int err;

	err = agfs_inode_path(sbi, ino, &ino_p);
	if (err) {
		atomic_dec(&sbi->staging_fd_count);
		return ERR_PTR(err);
	}
	if (flags & O_TRUNC) {
		err = vfs_truncate(&ino_p, 0);
		if (err) {
			path_put(&ino_p);
			atomic_dec(&sbi->staging_fd_count);
			return ERR_PTR(err);
		}
	}
	f = dentry_open(&ino_p, flags, current_cred());
	path_put(&ino_p);
	if (IS_ERR(f))
		atomic_dec(&sbi->staging_fd_count);
	return f;
}

/* Read ino + gen from the parent's dirent table. */
static void agfs_read_dirent(struct dentry *dentry, u64 *ino, u64 *gen)
{
	struct inode *dir = d_inode(dentry->d_parent);
	struct agfs_dirent *de;

	/* open() does not hold parent's i_rwsem — take it explicitly */
	inode_lock_shared(dir);
	de = agfs_find_dirent(dir, dentry->d_name.name,
				 dentry->d_name.len);
	if (de) {
		*ino = de->ino;
		*gen = de->gen;
	} else {
		*ino = 0;
		*gen = 0;
	}
	inode_unlock_shared(dir);
}

/* Open the right file for a staged regular file.
 * COW is resolved at open time — write_iter and mmap are pure pass-throughs.
 */
static struct file *agfs_open_staged(struct agfs_sb_info *sbi,
				     struct dentry *dentry,
				     struct file *file)
{
	struct file *new_file = NULL;
	bool truncate;
	u64 ino, gen;
	int err;

	agfs_read_dirent(dentry, &ino, &gen);

	if (!(file->f_flags & (O_WRONLY | O_RDWR)))
		return agfs_open_lower(dentry, file->f_flags);

	/* Fast path: inode is current — open directly */
	if (agfs_ino_is_staged(ino) &&
	    gen >= (u64)atomic64_read(&sbi->gen)) {
		down_read(&sbi->staging_sem);
		/* Re-check under lock — a checkpoint may have raced */
		if (gen >= (u64)atomic64_read(&sbi->gen)) {
			atomic_inc(&sbi->staging_fd_count);
			up_read(&sbi->staging_sem);
			return agfs_open_staged_ino(sbi, ino, file->f_flags);
		}
		up_read(&sbi->staging_sem);
	}

	/* Slow path: needs COW (base file, redirected, or stale inode) */
	truncate = !!(file->f_flags & O_TRUNC);

	down_write(&sbi->staging_sem);

	/* Re-check under sem — a concurrent open may have COW'd */
	agfs_read_dirent(dentry, &ino, &gen);
	if (agfs_ino_is_staged(ino) &&
	    gen >= (u64)atomic64_read(&sbi->gen)) {
		atomic_inc(&sbi->staging_fd_count);
		up_write(&sbi->staging_sem);
		return agfs_open_staged_ino(sbi, ino, file->f_flags);
	}

	atomic_inc(&sbi->staging_fd_count);
	err = agfs_do_cow(sbi, dentry, &new_file,
			  file->f_flags & ~O_TRUNC, truncate);
	up_write(&sbi->staging_sem);

	if (err) {
		atomic_dec(&sbi->staging_fd_count);
		return ERR_PTR(err);
	}
	return new_file;
}

/* ── open ──────────────────────────────────────────────────────────── */

static int agfs_open(struct inode *inode, struct file *file)
{
	struct agfs_sb_info *sbi = AGFS_SB(inode->i_sb);
	struct dentry *dentry = file->f_path.dentry;
	struct agfs_file_info *fi;
	struct file *lower_file;
	int err;

	fi = kzalloc(sizeof(*fi), GFP_KERNEL);
	if (!fi)
		return -ENOMEM;

	if (S_ISREG(inode->i_mode) && sbi->permission) {
		char buf[AGFS_PATH_MAX];

		err = agfs_check_open_perm(sbi, dentry, file, buf);
		if (err)
			goto out_free;
	}

	if (S_ISREG(inode->i_mode) && sbi->staging) {
		lower_file = agfs_open_staged(sbi, dentry, file);
	} else {
		lower_file = agfs_open_lower(dentry, file->f_flags);
	}

	if (IS_ERR(lower_file)) {
		err = PTR_ERR(lower_file);
		goto out_free;
	}

	fi->lower_file = lower_file;
	file->private_data = fi;
	return 0;

out_free:
	kfree(fi);
	return err;
}

/* ── read_iter ─────────────────────────────────────────────────────── */

static ssize_t agfs_read_iter(struct kiocb *iocb, struct iov_iter *iter)
{
	struct file *file = iocb->ki_filp;
	struct agfs_file_info *fi = AGFS_F(file);
	struct file *lower_file = fi->lower_file;
	ssize_t ret;

	if (!lower_file)
		return -EIO;

	get_file(lower_file);
	iocb->ki_filp = lower_file;
	ret = lower_file->f_op->read_iter(iocb, iter);
	iocb->ki_filp = file;
	fput(lower_file);

	if (ret >= 0 || ret == -EIOCBQUEUED)
		fsstack_copy_attr_atime(d_inode(file->f_path.dentry),
					file_inode(lower_file));
	return ret;
}

/* ── write_iter (pure pass-through — COW resolved at open time) ────── */

static ssize_t agfs_write_iter(struct kiocb *iocb, struct iov_iter *iter)
{
	struct file *file = iocb->ki_filp;
	struct agfs_file_info *fi = AGFS_F(file);
	struct file *lower_file;
	ssize_t ret;

	lower_file = fi->lower_file;
	if (!lower_file)
		return -EIO;

	get_file(lower_file);
	iocb->ki_filp = lower_file;
	ret = lower_file->f_op->write_iter(iocb, iter);
	iocb->ki_filp = file;
	fput(lower_file);

	if (ret >= 0 || ret == -EIOCBQUEUED) {
		fsstack_copy_inode_size(d_inode(file->f_path.dentry),
					file_inode(lower_file));
		fsstack_copy_attr_times(d_inode(file->f_path.dentry),
					file_inode(lower_file));
	}
	return ret;
}

/* ── mmap (pure pass-through — COW resolved at open time) ──────────── */

static int agfs_mmap(struct file *file, struct vm_area_struct *vma)
{
	struct agfs_file_info *fi = AGFS_F(file);
	struct file *lower_file;
	int err;

	lower_file = fi->lower_file;
	if (!lower_file)
		return -EIO;
	if (!lower_file->f_op || !lower_file->f_op->mmap)
		return -ENODEV;

	/* Save original vm_file and swap */
	get_file(lower_file);
	vma->vm_file = lower_file;
	err = lower_file->f_op->mmap(lower_file, vma);
	if (err) {
		fput(lower_file);
		vma->vm_file = file;
	} else {
		fput(file); /* balance the get_file in do_mmap */
	}
	return err;
}

/* ── fsync ─────────────────────────────────────────────────────────── */

static int agfs_fsync(struct file *file, loff_t start, loff_t end,
		      int datasync)
{
	struct agfs_file_info *fi = AGFS_F(file);
	struct file *lower_file = fi->lower_file;

	if (!lower_file)
		return -EIO;

	/* Staged writable files are ephemeral — skip fsync */
	if (AGFS_SB(file_inode(file)->i_sb)->staging &&
	    S_ISREG(file_inode(file)->i_mode) &&
	    (file->f_mode & FMODE_WRITE))
		return 0;

	return vfs_fsync_range(lower_file, start, end, datasync);
}

/* ── release ───────────────────────────────────────────────────────── */

static int agfs_release(struct inode *inode, struct file *file)
{
	struct agfs_file_info *fi = AGFS_F(file);
	struct agfs_sb_info *sbi = AGFS_SB(inode->i_sb);

	if (fi) {
		if (file == READ_ONCE(sbi->ask_engine.daemon_file))
			agfs_daemon_cleanup(sbi);

		/* Decrement staging fd count for write-mode opens */
		if (sbi->staging && S_ISREG(inode->i_mode) &&
		    (file->f_mode & FMODE_WRITE))
			atomic_dec(&sbi->staging_fd_count);

		if (fi->lower_file)
			fput(fi->lower_file);
		kfree(fi);
		file->private_data = NULL;
	}
	return 0;
}

/* ── llseek ────────────────────────────────────────────────────────── */

static loff_t agfs_llseek(struct file *file, loff_t offset, int whence)
{
	struct agfs_file_info *fi = AGFS_F(file);
	struct file *lower_file = fi->lower_file;
	loff_t ret;

	if (!lower_file)
		return -EIO;

	ret = vfs_llseek(lower_file, offset, whence);
	if (ret >= 0)
		file->f_pos = lower_file->f_pos;
	return ret;
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
	struct inode		*dir;
	struct dir_context	*caller_ctx;
	loff_t			*off;
};

static bool agfs_fill_base(struct dir_context *ctx, const char *name,
			   int namelen, loff_t offset, u64 ino,
			   unsigned int d_type)
{
	struct agfs_readdir_data *rdd =
		container_of(ctx, struct agfs_readdir_data, ctx);

	if (agfs_find_dirent(rdd->dir, name, namelen))
		return true;

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
 * Emit non-deleted dirent entries.
 * Caller holds inode_lock_shared(dir) via VFS iterate_shared.
 * Returns true if the dir_emit buffer filled up.
 */
static bool agfs_emit_dirents(struct inode *dir, struct dir_context *ctx,
			      loff_t *off)
{
	struct agfs_inode_info *dii = AGFS_I(dir);
	struct agfs_dirent *de;
	unsigned int bi;

	if (!dii->de_buckets)
		return false;

	for (bi = 0; bi < AGFS_DE_BUCKETS; bi++) {
		hlist_for_each_entry(de, &dii->de_buckets[bi], node) {
			if (agfs_ino_is_deleted(de->ino)) {
				(*off)++;
				continue;
			}
			if (*off < ctx->pos) {
				(*off)++;
				continue;
			}

			if (!dir_emit(ctx, de->name, de->name_len,
				      de->ino, de->d_type))
				return true;
			(*off)++;
			ctx->pos++;
		}
	}
	return false;
}

/* ── readdir entry point ───────────────────────────────────────────── */

static int agfs_readdir(struct file *file, struct dir_context *ctx)
{
	struct agfs_file_info *fi = AGFS_F(file);
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);
	struct file *lower_file = fi->lower_file;
	struct agfs_readdir_data rdd;
	loff_t off = 0;
	int err = 0;

	if (!lower_file)
		return -EIO;

	/* No staging → simple passthrough */
	if (!sbi->staging || !sbi->inodes_dir.dentry) {
		lower_file->f_pos = ctx->pos;
		err = iterate_dir(lower_file, ctx);
		file->f_pos = lower_file->f_pos;
		return err;
	}

	/* Phase 1: emit non-deleted dirent entries */
	if (agfs_emit_dirents(file_inode(file), ctx, &off))
		return 0;

	/* Phase 2: read base directory, skip overridden names */
	rdd.ctx.actor = agfs_fill_base;
	rdd.ctx.pos = 0;
	rdd.dir = file_inode(file);
	rdd.caller_ctx = ctx;
	rdd.off = &off;

	lower_file->f_pos = 0;
	err = iterate_dir(lower_file, &rdd.ctx);

	return err;
}

static int agfs_dir_open(struct inode *inode, struct file *file)
{
	struct agfs_file_info *fi;
	struct file *lower_file;
	struct path lower_path;

	fi = kzalloc(sizeof(*fi), GFP_KERNEL);
	if (!fi)
		return -ENOMEM;

	agfs_get_lower_path(file->f_path.dentry, &lower_path);
	lower_file = dentry_open(&lower_path, file->f_flags, current_cred());
	agfs_put_lower_path(file->f_path.dentry, &lower_path);
	if (IS_ERR(lower_file)) {
		kfree(fi);
		return PTR_ERR(lower_file);
	}

	fi->lower_file = lower_file;
	file->private_data = fi;
	return 0;
}

/* ── Address-Space Ops (minimal) ───────────────────────────────────── */

static ssize_t agfs_direct_IO(struct kiocb *iocb, struct iov_iter *iter)
{
	return -EINVAL;
}

const struct address_space_operations agfs_aops = {
	.direct_IO = agfs_direct_IO,
};

/* ── fallocate (pass-through to lower file) ────────────────────────── */

static long agfs_fallocate(struct file *file, int mode, loff_t offset, loff_t len)
{
	struct agfs_file_info *fi = AGFS_F(file);
	struct file *lower_file = fi ? fi->lower_file : NULL;

	if (!lower_file)
		return -EIO;

	if (!lower_file->f_op || !lower_file->f_op->fallocate)
		return -EOPNOTSUPP;

	return lower_file->f_op->fallocate(lower_file, mode, offset, len);
}

/* ── File Ops Tables ───────────────────────────────────────────────── */

const struct file_operations agfs_main_fops = {
	.open		= agfs_open,
	.release	= agfs_release,
	.read_iter	= agfs_read_iter,
	.write_iter	= agfs_write_iter,
	.fallocate	= agfs_fallocate,
	.llseek		= agfs_llseek,
	.mmap		= agfs_mmap,
	.fsync		= agfs_fsync,
};

const struct file_operations agfs_dir_fops = {
	.open		= agfs_dir_open,
	.release	= agfs_release,
	.iterate_shared	= agfs_readdir,
	.llseek		= agfs_llseek,
	.fsync		= agfs_fsync,
	.unlocked_ioctl	= agfs_ioctl,
	.compat_ioctl	= agfs_ioctl,
};
