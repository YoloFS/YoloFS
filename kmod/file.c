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

/* ── open (§3.5 + §4.3) ───────────────────────────────────────────── */

static int agfs_open(struct inode *inode, struct file *file)
{
	struct agfs_sb_info *sbi = AGFS_SB(inode->i_sb);
	struct agfs_file_info *fi;
	struct dentry *dentry = file->f_path.dentry;
	struct file *lower_file = NULL;
	const struct cred *old_cred = NULL;
	char buf[AGFS_PATH_MAX];
	int err;

	fi = kzalloc(sizeof(*fi), GFP_KERNEL);
	if (!fi)
		return -ENOMEM;

	/* ── Permission gating for regular files ────────────────────── */
	if (S_ISREG(inode->i_mode) && !sbi->noperm) {
		enum agfs_perm perm = AGFS_I(inode)->cached_perm;

		/* Re-resolve if stale */
		if (AGFS_I(inode)->perm_gen !=
		    atomic64_read(&sbi->perm_gen)) {
			perm = agfs_resolve_perm(dentry);
			AGFS_I(inode)->cached_perm = perm;
			AGFS_I(inode)->perm_gen =
				atomic64_read(&sbi->perm_gen);
		}

		if (perm == AGFS_PERM_ASK) {
			unsigned int op;
			if (file->f_mode & FMODE_EXEC)
				op = AGFS_OP_EXEC;
			else if (file->f_flags & (O_WRONLY | O_RDWR | O_APPEND | O_TRUNC))
				op = AGFS_OP_WRITE;
			else
				op = AGFS_OP_READ;

			err = agfs_dentry_relpath(dentry, buf, sizeof(buf));
			if (err)
				goto out_free;
			err = agfs_ask_userspace(sbi, dentry, buf, op, &perm);
			if (err)
				goto out_free;
		}

		err = agfs_check_perm(perm, file->f_flags);
		if (err) {
			agfs_log_emit(sbi, AGFS_LOG_DENY, perm, 0,
				      dentry->d_name.name, 0);
			goto out_free;
		}
	}

	/* ── Staging redirect for regular files ─────────────────────── */
	if (S_ISREG(inode->i_mode) && !sbi->nostaging) {
		old_cred = override_creds(sbi->creator_cred);

		err = agfs_dentry_relpath(dentry, buf, sizeof(buf));
		if (err)
			goto out_free;

		if (file->f_flags & O_TRUNC) {
			/* Truncating write — create empty staging file */
			err = agfs_create_staging_parents(sbi, buf);
			if (err)
				goto out_free;

			{
				struct path staging;
				/* Try to open existing staging file first */
				err = agfs_staging_path(sbi, buf, &staging);
				if (!err) {
					lower_file = dentry_open(
						&staging, file->f_flags,
						current_cred());
					path_put(&staging);
				} else {
					/* No staging file: create empty one
					 * (no COW needed — content is being
					 * truncated anyway) */
					err = agfs_create_staging_empty(
						sbi, buf, &lower_file,
						file->f_flags);
				}
			}
			if (IS_ERR(lower_file)) {
				err = PTR_ERR(lower_file);
				lower_file = NULL;
				goto out_free;
			}
			if (err)
				goto out_free;
			fi->needs_cow = false;
			fi->is_staging = true;
			/* We handled truncation ourselves — prevent the
			 * VFS from calling handle_truncate() which would
			 * truncate the BASE file via agfs_setattr. */
			file->f_flags &= ~O_TRUNC;
		} else if (file->f_flags & (O_WRONLY | O_RDWR)) {
			if (agfs_staging_has(sbi, buf)) {
				/* Already in staging */
				struct path staging;
				err = agfs_staging_path(sbi, buf, &staging);
				if (err)
					goto out_free;
				lower_file = dentry_open(&staging,
							 file->f_flags,
							 current_cred());
				path_put(&staging);
				if (IS_ERR(lower_file)) {
					err = PTR_ERR(lower_file);
					lower_file = NULL;
					goto out_free;
				}
				fi->needs_cow = false;
				fi->is_staging = true;
			} else {
				/* Open base read-only; COW on first write */
				struct path lower_path;
				agfs_get_lower_path(dentry, &lower_path);
				lower_file = dentry_open(&lower_path,
							 O_RDONLY,
							 current_cred());
				agfs_put_lower_path(dentry, &lower_path);
				if (IS_ERR(lower_file)) {
					err = PTR_ERR(lower_file);
					lower_file = NULL;
					goto out_free;
				}
				fi->needs_cow = true;
			}
		} else {
			/* Read-only: prefer staging, fall back to base */
			if (agfs_staging_has(sbi, buf)) {
				struct path staging;
				err = agfs_staging_path(sbi, buf, &staging);
				if (err) {
					revert_creds(old_cred);
					old_cred = NULL;
					goto open_lower;
				}
				lower_file = dentry_open(&staging,
							 file->f_flags,
							 current_cred());
				path_put(&staging);
				if (IS_ERR(lower_file)) {
					err = PTR_ERR(lower_file);
					lower_file = NULL;
					goto out_free;
				}
			} else {
				revert_creds(old_cred);
				old_cred = NULL;
				goto open_lower;
			}
			fi->needs_cow = false;
			fi->is_staging = true;
		}

		revert_creds(old_cred);
		old_cred = NULL;
		goto done;
	}

open_lower:
	/* Default: open lower file directly */
	{
		struct path lower_path;
		agfs_get_lower_path(dentry, &lower_path);
		lower_file = dentry_open(&lower_path, file->f_flags,
					 current_cred());
		agfs_put_lower_path(dentry, &lower_path);
		if (IS_ERR(lower_file)) {
			err = PTR_ERR(lower_file);
			lower_file = NULL;
			goto out_free;
		}
		fi->needs_cow = false;
	}

done:
	fi->lower_file = lower_file;
	file->private_data = fi;

	agfs_log_emit(sbi, AGFS_LOG_OPEN, AGFS_I(inode)->cached_perm,
		      AGFS_OP_OPEN, dentry->d_name.name, 0);
	return 0;

out_free:
	if (old_cred)
		revert_creds(old_cred);
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

/* ── write_iter (§3.5) ─────────────────────────────────────────────── */

static ssize_t agfs_write_iter(struct kiocb *iocb, struct iov_iter *iter)
{
	struct file *file = iocb->ki_filp;
	struct agfs_file_info *fi = AGFS_F(file);
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);
	struct file *lower_file;
	ssize_t ret;

	/* Lazy COW: copy base → staging on first write */
	if (fi->needs_cow) {
		const struct cred *old_cred;
		char buf[AGFS_PATH_MAX];
		struct file *new_file = NULL;
		int err;

		err = agfs_dentry_relpath(file->f_path.dentry, buf,
					  sizeof(buf));
		if (err)
			return err;

		old_cred = override_creds(sbi->creator_cred);
		down_write(&sbi->staging_sem);
		/* Re-check after acquiring lock (another thread may have COW'd) */
		if (!fi->needs_cow) {
			up_write(&sbi->staging_sem);
			revert_creds(old_cred);
			goto cow_done;
		}
		err = agfs_do_cow(sbi, buf, &new_file,
				  file->f_flags & ~O_TRUNC);
		if (err) {
			up_write(&sbi->staging_sem);
			revert_creds(old_cred);
			return err;
		}

		fput(fi->lower_file);
		fi->lower_file = new_file;
		fi->needs_cow = false;
		fi->is_staging = true;
		up_write(&sbi->staging_sem);
		revert_creds(old_cred);
	}

cow_done:
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

/* ── mmap ──────────────────────────────────────────────────────────── */

static int agfs_mmap(struct file *file, struct vm_area_struct *vma)
{
	struct agfs_file_info *fi = AGFS_F(file);
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);
	struct file *lower_file;
	int err;

	/*
	 * Writable shared mapping on a file that still needs COW:
	 * the lower file was opened O_RDONLY, so the kernel would
	 * reject mmap(PROT_WRITE, MAP_SHARED) with -EACCES.
	 * Trigger COW now to get a writable staging file.
	 */
	if (fi->needs_cow &&
	    (vma->vm_flags & (VM_WRITE | VM_SHARED)) ==
	    (VM_WRITE | VM_SHARED)) {
		const struct cred *old_cred;
		char buf[AGFS_PATH_MAX];
		struct file *new_file = NULL;

		err = agfs_dentry_relpath(file->f_path.dentry, buf,
					  sizeof(buf));
		if (err)
			return err;

		old_cred = override_creds(sbi->creator_cred);
		down_write(&sbi->staging_sem);
		if (!fi->needs_cow) {
			up_write(&sbi->staging_sem);
			revert_creds(old_cred);
			goto mmap_ready;
		}
		err = agfs_do_cow(sbi, buf, &new_file,
				  file->f_flags & ~O_TRUNC);
		if (err) {
			up_write(&sbi->staging_sem);
			revert_creds(old_cred);
			return err;
		}

		fput(fi->lower_file);
		fi->lower_file = new_file;
		fi->needs_cow = false;
		fi->is_staging = true;
		up_write(&sbi->staging_sem);
		revert_creds(old_cred);
	}

mmap_ready:
	lower_file = fi->lower_file;
	if (!lower_file)
		return -EIO;
	if (!lower_file->f_op->mmap)
		return -ENODEV;

	/* Save original vm_file and swap */
	get_file(lower_file);
	vma->vm_file = lower_file;
	err = lower_file->f_op->mmap(lower_file, vma);
	if (err) {
		fput(lower_file);
		vma->vm_file = file;
	} else {
		fi->lower_vm_ops = vma->vm_ops;
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

	/* Staging files are ephemeral — no point fsyncing them */
	if (fi->is_staging)
		return 0;

	return vfs_fsync_range(lower_file, start, end, datasync);
}

/* ── release ───────────────────────────────────────────────────────── */

static int agfs_release(struct inode *inode, struct file *file)
{
	struct agfs_file_info *fi = AGFS_F(file);

	if (fi) {
		if (fi->ctl)
			agfs_ctl_cleanup(AGFS_SB(inode->i_sb), fi->ctl);
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

/* ── Directory: readdir / iterate_shared ───────────────────────────── */

static int agfs_readdir(struct file *file, struct dir_context *ctx)
{
	struct agfs_file_info *fi = AGFS_F(file);
	struct file *lower_file = fi->lower_file;
	int err;

	if (!lower_file)
		return -EIO;

	lower_file->f_pos = ctx->pos;
	err = iterate_dir(lower_file, ctx);
	file->f_pos = lower_file->f_pos;
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

/* ── File Ops Tables ───────────────────────────────────────────────── */

const struct file_operations agfs_main_fops = {
	.open		= agfs_open,
	.release	= agfs_release,
	.read_iter	= agfs_read_iter,
	.write_iter	= agfs_write_iter,
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
