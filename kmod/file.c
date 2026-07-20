// SPDX-License-Identifier: GPL-2.0
/*
 * yolofs — regular-file operations.
 *
 * open (perm gating + staging redirect), read_iter, write_iter,
 * mmap, release, llseek, fallocate.
 */

#include "yolofs.h"
#include <linux/file.h>
#include <linux/mm.h>

/* ── open helpers ───────────────────────────────────────────────────── */

static struct file *yolo_open_lower(struct dentry *dentry, int flags)
{
	struct path lower_path;
	struct file *f;

	yolo_get_lower_path(dentry, &lower_path);
	f = dentry_open(&lower_path, flags, current_cred());
	yolo_put_lower_path(dentry, &lower_path);
	return f;
}

/*
 * Open a staged inode via lower_path, incrementing staging_fd_count.
 * On error, decrements the count and returns ERR_PTR.
 *
 * dentry_open() does not apply O_TRUNC (that is normally done by the
 * VFS after f_op->open returns), and yolo_setattr intentionally strips
 * ATTR_SIZE for staged files.  So we must truncate the lower inode
 * ourselves before opening.
 */
static struct file *yolo_open_staged_lower(struct dentry *dentry,
					   struct yolo_sb_info *sbi,
					   int flags)
{
	struct path lower_path;
	struct file *f;
	int err;

	yolo_get_lower_path(dentry, &lower_path);
	if ((flags & O_TRUNC) && i_size_read(d_inode(lower_path.dentry))) {
		err = vfs_truncate(&lower_path, 0);
		if (err) {
			yolo_put_lower_path(dentry, &lower_path);
			atomic_dec(&sbi->staging.fd_count);
			return ERR_PTR(err);
		}
	}
	f = dentry_open(&lower_path, flags, current_cred());
	yolo_put_lower_path(dentry, &lower_path);
	if (IS_ERR(f))
		atomic_dec(&sbi->staging.fd_count);
	return f;
}

/* Open the right file for a staged regular file.
 * COW is resolved at open time — write_iter and mmap are pure pass-throughs.
 */
static struct file *yolo_open_staged(struct yolo_sb_info *sbi,
				     struct dentry *dentry,
				     struct file *file)
{
	struct file *new_file = NULL;
	bool truncate;
	int err;

	if (!(file->f_flags & (O_WRONLY | O_RDWR)))
		return yolo_open_lower(dentry, file->f_flags);

	/* Fast path: inode is current — open directly.
	 * staging_sem excludes snapshot, so gen is stable under the lock. */
	down_read(&sbi->staging.sem);
	if (yolo_dentry_is_current(dentry, sbi)) {
		atomic_inc(&sbi->staging.fd_count);
		up_read(&sbi->staging.sem);
		return yolo_open_staged_lower(dentry, sbi, file->f_flags);
	}
	up_read(&sbi->staging.sem);

	/* Slow path: needs COW (base file, redirect, or stale inode) */
	truncate = !!(file->f_flags & O_TRUNC);

	down_write(&sbi->staging.sem);

	/* Re-check — a concurrent open may have COW'd */
	if (yolo_dentry_is_current(dentry, sbi)) {
		atomic_inc(&sbi->staging.fd_count);
		up_write(&sbi->staging.sem);
		return yolo_open_staged_lower(dentry, sbi, file->f_flags);
	}

	atomic_inc(&sbi->staging.fd_count);
	err = yolo_do_cow(sbi, dentry, &new_file,
			  file->f_flags & ~O_TRUNC, truncate);
	up_write(&sbi->staging.sem);

	if (err) {
		atomic_dec(&sbi->staging.fd_count);
		return ERR_PTR(err);
	}
	return new_file;
}

/* ── open ──────────────────────────────────────────────────────────── */

/* Map VFS open flags to the operation being attempted (read vs write). */
static enum yolo_op yolo_open_op(int f_flags)
{
	if (f_flags & (O_WRONLY | O_RDWR | O_APPEND | O_TRUNC))
		return YOLO_OP_WRITE;
	return YOLO_OP_READ;
}

static int yolo_open(struct inode *inode, struct file *file)
{
	struct yolo_sb_info *sbi = YOLO_SB(inode->i_sb);
	struct dentry *dentry = file->f_path.dentry;
	struct yolo_file_info *fi;
	struct file *lower_file;
	int err;

	if (sbi->perm.enabled) {
		/* check == target: the file's own access gates its open. */
		err = yolo_perm_check_dentry(sbi, dentry, dentry,
					     yolo_open_op(file->f_flags));
		if (err)
			return err;
	}

	fi = kzalloc(sizeof(*fi), GFP_KERNEL);
	if (!fi)
		return -ENOMEM;

	if (sbi->staging.enabled) {
		lower_file = yolo_open_staged(sbi, dentry, file);
	} else {
		lower_file = yolo_open_lower(dentry, file->f_flags);
	}

	if (IS_ERR(lower_file)) {
		kfree(fi);
		return PTR_ERR(lower_file);
	}

	fi->lower_file = lower_file;
	file->private_data = fi;
	return 0;
}

/* ── read_iter ─────────────────────────────────────────────────────── */

static ssize_t yolo_read_iter(struct kiocb *iocb, struct iov_iter *iter)
{
	struct file *file = iocb->ki_filp;
	struct yolo_file_info *fi = YOLO_F(file);
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

static ssize_t yolo_write_iter(struct kiocb *iocb, struct iov_iter *iter)
{
	struct file *file = iocb->ki_filp;
	struct yolo_file_info *fi = YOLO_F(file);
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

static int yolo_mmap(struct file *file, struct vm_area_struct *vma)
{
	struct yolo_file_info *fi = YOLO_F(file);
	struct file *lower_file;
	int err;

	lower_file = fi->lower_file;
	if (!lower_file)
		return -EIO;
	if (!lower_file->f_op)
		return -ENODEV;

	/* Save original vm_file and swap to the lower before delegating. */
	get_file(lower_file);
	vma->vm_file = lower_file;

	/*
	 * 7.0 split f_op->mmap into the pre-vma f_op->mmap_prepare hook, and ext4
	 * (our usual lower fs) moved to it. vfs_mmap() dispatches to whichever hook
	 * the lower provides (can_mmap_file() rejects a lower with neither); on 6.8
	 * only ->mmap exists. -ENODEV if the lower can't mmap; the err path below
	 * drops the ref we just took.
	 */
#if LINUX_VERSION_CODE >= KERNEL_VERSION(7, 0, 0)
	err = can_mmap_file(lower_file) ? vfs_mmap(lower_file, vma) : -ENODEV;
#else
	err = lower_file->f_op->mmap ? lower_file->f_op->mmap(lower_file, vma) : -ENODEV;
#endif
	if (err) {
		fput(lower_file);
		vma->vm_file = file;
	} else {
		fput(file); /* balance the get_file in do_mmap */
	}
	return err;
}

/* ── release ───────────────────────────────────────────────────────── */

static int yolo_release(struct inode *inode, struct file *file)
{
	struct yolo_file_info *fi = YOLO_F(file);

	if (fi) {
		struct yolo_sb_info *sbi = YOLO_SB(inode->i_sb);

		/* Decrement staging fd count for write-mode opens */
		if (sbi->staging.enabled && (file->f_mode & FMODE_WRITE))
			atomic_dec(&sbi->staging.fd_count);

		if (fi->lower_file)
			fput(fi->lower_file);
		kfree(fi);
		file->private_data = NULL;
	}
	return 0;
}

/* ── llseek ────────────────────────────────────────────────────────── */

static loff_t yolo_llseek(struct file *file, loff_t offset, int whence)
{
	struct yolo_file_info *fi = YOLO_F(file);
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

static ssize_t yolo_direct_IO(struct kiocb *iocb, struct iov_iter *iter)
{
	return -EINVAL;
}

const struct address_space_operations yolo_aops = {
	.direct_IO = yolo_direct_IO,
};

/* ── fallocate (pass-through to lower file) ────────────────────────── */

static long yolo_fallocate(struct file *file, int mode, loff_t offset, loff_t len)
{
	struct yolo_file_info *fi = YOLO_F(file);
	struct file *lower_file = fi ? fi->lower_file : NULL;

	if (!lower_file)
		return -EIO;

	if (!lower_file->f_op || !lower_file->f_op->fallocate)
		return -EOPNOTSUPP;

	return lower_file->f_op->fallocate(lower_file, mode, offset, len);
}

/* ── File Ops Table ────────────────────────────────────────────────── */

const struct file_operations yolo_main_fops = {
	.open		= yolo_open,
	.release	= yolo_release,
	.read_iter	= yolo_read_iter,
	.write_iter	= yolo_write_iter,
	.fallocate	= yolo_fallocate,
	.llseek		= yolo_llseek,
	.mmap		= yolo_mmap,
	.fsync		= noop_fsync,
};
