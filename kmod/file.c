// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — regular-file operations.
 *
 * open (perm gating + staging redirect), read_iter, write_iter,
 * mmap, release, llseek, fallocate.
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
	struct inode *inode = d_inode(dentry);
	struct agfs_inode_info *ii = AGFS_I(inode);
	enum agfs_perm perm;
	int err;

	if (ii->perm_gen != atomic64_read(&sbi->perm_gen))
		agfs_cache_perm(inode, dentry);
	perm = ii->cached_perm;

	if (perm == AGFS_PERM_ASK) {
		unsigned int op;
		char *relpath;

		if (file->f_mode & FMODE_EXEC)
			op = AGFS_OP_EXEC;
		else if (file->f_flags & (O_WRONLY | O_RDWR | O_APPEND | O_TRUNC))
			op = AGFS_OP_WRITE;
		else
			op = AGFS_OP_READ;

		relpath = dentry_path_raw(dentry, buf, AGFS_PATH_MAX);
		if (IS_ERR(relpath))
			return PTR_ERR(relpath);
		err = agfs_ask_userspace(sbi, dentry, relpath, op, &perm);
		if (err)
			return err;
	}

	return agfs_check_perm(perm, file->f_flags);
}

/*
 * Open a staged inode via lower_path, incrementing staging_fd_count.
 * On error, decrements the count and returns ERR_PTR.
 *
 * dentry_open() does not apply O_TRUNC (that is normally done by the
 * VFS after f_op->open returns), and agfs_setattr intentionally strips
 * ATTR_SIZE for staged files.  So we must truncate the lower inode
 * ourselves before opening.
 */
static struct file *agfs_open_staged_lower(struct dentry *dentry,
					   struct agfs_sb_info *sbi,
					   int flags)
{
	struct path lower_path;
	struct file *f;
	int err;

	agfs_get_lower_path(dentry, &lower_path);
	if ((flags & O_TRUNC) && i_size_read(d_inode(lower_path.dentry))) {
		err = vfs_truncate(&lower_path, 0);
		if (err) {
			agfs_put_lower_path(dentry, &lower_path);
			atomic_dec(&sbi->staging_fd_count);
			return ERR_PTR(err);
		}
	}
	f = dentry_open(&lower_path, flags, current_cred());
	agfs_put_lower_path(dentry, &lower_path);
	if (IS_ERR(f))
		atomic_dec(&sbi->staging_fd_count);
	return f;
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
	int err;

	if (!(file->f_flags & (O_WRONLY | O_RDWR)))
		return agfs_open_lower(dentry, file->f_flags);

	/* Fast path: inode is current — open directly.
	 * staging_sem excludes mark, so gen is stable under the lock. */
	down_read(&sbi->staging_sem);
	if (agfs_dentry_is_current(dentry, sbi)) {
		atomic_inc(&sbi->staging_fd_count);
		up_read(&sbi->staging_sem);
		return agfs_open_staged_lower(dentry, sbi, file->f_flags);
	}
	up_read(&sbi->staging_sem);

	/* Slow path: needs COW (base file, redirect, or stale inode) */
	truncate = !!(file->f_flags & O_TRUNC);

	down_write(&sbi->staging_sem);

	/* Re-check — a concurrent open may have COW'd */
	if (agfs_dentry_is_current(dentry, sbi)) {
		atomic_inc(&sbi->staging_fd_count);
		up_write(&sbi->staging_sem);
		return agfs_open_staged_lower(dentry, sbi, file->f_flags);
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

	if (sbi->permission) {
		char buf[AGFS_PATH_MAX];

		err = agfs_check_open_perm(sbi, dentry, file, buf);
		if (err)
			goto out_free;
	}

	if (sbi->staging) {
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

/* ── release ───────────────────────────────────────────────────────── */

static int agfs_release(struct inode *inode, struct file *file)
{
	struct agfs_file_info *fi = AGFS_F(file);

	if (fi) {
		struct agfs_sb_info *sbi = AGFS_SB(inode->i_sb);

		/* Decrement staging fd count for write-mode opens */
		if (sbi->staging && (file->f_mode & FMODE_WRITE))
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

	/* Sync lower file position before seeking — write_iter updates
	 * iocb->ki_pos (which the VFS copies to file->f_pos) but does
	 * not update lower_file->f_pos directly.  Without this,
	 * SEEK_CUR after a write uses a stale lower position. */
	lower_file->f_pos = file->f_pos;

	ret = vfs_llseek(lower_file, offset, whence);
	if (ret >= 0)
		file->f_pos = lower_file->f_pos;
	return ret;
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

/* ── File Ops Table ────────────────────────────────────────────────── */

const struct file_operations agfs_main_fops = {
	.open		= agfs_open,
	.release	= agfs_release,
	.read_iter	= agfs_read_iter,
	.write_iter	= agfs_write_iter,
	.fallocate	= agfs_fallocate,
	.llseek		= agfs_llseek,
	.mmap		= agfs_mmap,
	.fsync		= noop_fsync,
};
