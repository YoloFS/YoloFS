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
		if (err)
			goto out_free;
	}

	/* ── Staging redirect for regular files ─────────────────────── */
	if (S_ISREG(inode->i_mode) && !sbi->nostaging) {
		struct agfs_dentry_info *parent_di = AGFS_D(dentry->d_parent);
		struct agfs_override *ovr;
		u64 sid = 0;

		old_cred = override_creds(sbi->creator_cred);

		/* Check override list to know if file is staged */
		if (parent_di) {
			spin_lock(&parent_di->lock);
			ovr = agfs_find_override(dentry->d_parent,
						 dentry->d_name.name,
						 dentry->d_name.len);
			if (ovr)
				sid = ovr->staging_id;
			spin_unlock(&parent_di->lock);
		}

		err = agfs_dentry_relpath(dentry, buf, sizeof(buf));
		if (err)
			goto out_free;

		if (file->f_flags & O_TRUNC) {
			/* Truncating write — allocate new staging blob */
			struct path blob_path;
			u64 id;

			err = agfs_staging_alloc(sbi, &id, &blob_path,
						 0644, NULL);
			if (err)
				goto out_free;

			lower_file = dentry_open(&blob_path,
						 file->f_flags & ~O_TRUNC,
						 current_cred());
			if (IS_ERR(lower_file)) {
				err = PTR_ERR(lower_file);
				lower_file = NULL;
				path_put(&blob_path);
				goto out_free;
			}

			/* Update override + dentry + journal */
			agfs_add_override(dentry->d_parent,
					  dentry->d_name.name,
					  dentry->d_name.len, id, NULL);
			agfs_set_lower_path(dentry, &blob_path);
			agfs_journal_append_a(sbi, buf, id);

			fi->needs_cow = false;
			fi->is_staging = true;
			file->f_flags &= ~O_TRUNC;
		} else if (file->f_flags & (O_WRONLY | O_RDWR)) {
			if (sid) {
				/* Already in staging — open the blob */
				struct path blob;
				err = agfs_staging_blob_path(sbi, sid, &blob);
				if (err)
					goto out_free;
				lower_file = dentry_open(&blob,
							 file->f_flags,
							 current_cred());
				path_put(&blob);
				if (IS_ERR(lower_file)) {
					err = PTR_ERR(lower_file);
					lower_file = NULL;
					goto out_free;
				}
				fi->needs_cow = false;
				fi->is_staging = true;
			} else {
				/* Base file — open read-only, COW on first write */
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
			/* Read-only: open the resolved lower file */
			struct path lower_path;
			agfs_get_lower_path(dentry, &lower_path);
			lower_file = dentry_open(&lower_path,
						 file->f_flags,
						 current_cred());
			agfs_put_lower_path(dentry, &lower_path);
			if (IS_ERR(lower_file)) {
				err = PTR_ERR(lower_file);
				lower_file = NULL;
				goto out_free;
			}
			fi->needs_cow = false;
		}

		revert_creds(old_cred);
		old_cred = NULL;
		goto done;
	}

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

	/* Lazy COW: copy base → staging blob on first write */
	if (fi->needs_cow) {
		const struct cred *old_cred;
		struct file *new_file = NULL;
		int err;

		old_cred = override_creds(sbi->creator_cred);
		down_write(&sbi->staging_sem);
		/* Re-check after acquiring lock (another thread may have COW'd) */
		if (!fi->needs_cow) {
			up_write(&sbi->staging_sem);
			revert_creds(old_cred);
			goto cow_done;
		}
		err = agfs_do_cow_blob(sbi, file->f_path.dentry, &new_file,
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
		struct file *new_file = NULL;

		old_cred = override_creds(sbi->creator_cred);
		down_write(&sbi->staging_sem);
		if (!fi->needs_cow) {
			up_write(&sbi->staging_sem);
			revert_creds(old_cred);
			goto mmap_ready;
		}
		err = agfs_do_cow_blob(sbi, file->f_path.dentry, &new_file,
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

/* ── Directory: readdir / iterate_shared (§3.8 merged listing) ──────── */

/*
 * Merged readdir: override entries first, then base entries not overridden.
 *
 * Override names are snapshotted into a hash set (for O(1) dedup), then
 * base directory entries are streamed through a filldir callback that
 * skips any name present in the set.
 */

/* ── Name hash set (open-addressed, power-of-two) ──────────────────── */

struct agfs_nameset {
	unsigned int		shift;		/* log2(capacity) */
	unsigned int		count;
	struct hlist_head	buckets[];
};

struct agfs_nameset_entry {
	struct hlist_node	node;
	unsigned int		len;
	bool			is_deleted;
	char			name[];
};

static struct agfs_nameset *agfs_nameset_alloc(unsigned int hint)
{
	struct agfs_nameset *ns;
	unsigned int shift = 4;	/* minimum 16 buckets */
	unsigned int i;

	while ((1u << shift) < hint * 2 && shift < 16)
		shift++;

	ns = kvzalloc(sizeof(*ns) + (sizeof(struct hlist_head) << shift),
		      GFP_KERNEL);
	if (!ns)
		return NULL;
	ns->shift = shift;
	ns->count = 0;
	for (i = 0; i < (1u << shift); i++)
		INIT_HLIST_HEAD(&ns->buckets[i]);
	return ns;
}

static void agfs_nameset_free(struct agfs_nameset *ns)
{
	unsigned int i;
	struct agfs_nameset_entry *e;
	struct hlist_node *tmp;

	if (!ns)
		return;
	for (i = 0; i < (1u << ns->shift); i++) {
		hlist_for_each_entry_safe(e, tmp, &ns->buckets[i], node)
			kfree(e);
	}
	kvfree(ns);
}

static unsigned int agfs_name_hash(const char *name, unsigned int len,
				   unsigned int shift)
{
	return full_name_hash(NULL, name, len) >> (32 - shift);
}

static bool agfs_nameset_has(struct agfs_nameset *ns,
			     const char *name, unsigned int len)
{
	unsigned int idx = agfs_name_hash(name, len, ns->shift);
	struct agfs_nameset_entry *e;

	hlist_for_each_entry(e, &ns->buckets[idx], node) {
		if (e->len == len && !memcmp(e->name, name, len))
			return true;
	}
	return false;
}

static int agfs_nameset_add(struct agfs_nameset *ns,
			    const char *name, unsigned int len,
			    bool is_deleted)
{
	unsigned int idx = agfs_name_hash(name, len, ns->shift);
	struct agfs_nameset_entry *e;

	e = kmalloc(offsetof(struct agfs_nameset_entry, name) + len + 1,
		    GFP_ATOMIC);
	if (!e)
		return -ENOMEM;
	memcpy(e->name, name, len);
	e->name[len] = '\0';
	e->len = len;
	e->is_deleted = is_deleted;
	hlist_add_head(&e->node, &ns->buckets[idx]);
	ns->count++;
	return 0;
}

/* ── filldir callback for base directory reading ───────────────────── */

struct agfs_readdir_data {
	struct dir_context	ctx;
	struct agfs_nameset	*ns;
	struct dir_context	*caller_ctx;
	loff_t			*off;
	int			err;
};

static bool agfs_fill_base(struct dir_context *ctx, const char *name,
			   int namelen, loff_t offset, u64 ino,
			   unsigned int d_type)
{
	struct agfs_readdir_data *rdd =
		container_of(ctx, struct agfs_readdir_data, ctx);

	if (agfs_nameset_has(rdd->ns, name, namelen))
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

/* ── readdir entry point ───────────────────────────────────────────── */

static int agfs_readdir(struct file *file, struct dir_context *ctx)
{
	struct agfs_file_info *fi = AGFS_F(file);
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);
	struct file *lower_file = fi->lower_file;
	struct agfs_dentry_info *di;
	struct agfs_nameset *ns = NULL;
	struct agfs_readdir_data rdd;
	loff_t off = 0;
	int err = 0;

	if (!lower_file)
		return -EIO;

	/* No staging → simple passthrough */
	if (sbi->nostaging || !sbi->staging_dir.dentry) {
		lower_file->f_pos = ctx->pos;
		err = iterate_dir(lower_file, ctx);
		file->f_pos = lower_file->f_pos;
		return err;
	}

	di = AGFS_D(file->f_path.dentry);

	/* Phase 1: snapshot override names into hash set, emit non-deleted */
	if (di) {
		struct agfs_override *ovr;
		unsigned int count = 0;

		spin_lock(&di->lock);
		list_for_each_entry(ovr, &di->overrides, list)
			count++;

		ns = agfs_nameset_alloc(count);
		if (!ns) {
			spin_unlock(&di->lock);
			return -ENOMEM;
		}

		list_for_each_entry(ovr, &di->overrides, list) {
			bool deleted = !ovr->staging_id && !ovr->base_path;

			err = agfs_nameset_add(ns, ovr->name, ovr->name_len,
					       deleted);
			if (err) {
				spin_unlock(&di->lock);
				goto out;
			}
		}
		spin_unlock(&di->lock);

		/* Emit non-deleted overrides, respecting ctx->pos */
		{
			unsigned int i;

			for (i = 0; i < (1u << ns->shift); i++) {
				struct agfs_nameset_entry *e;

				hlist_for_each_entry(e, &ns->buckets[i], node) {
					if (e->is_deleted) {
						off++;
						continue;
					}
					if (off < ctx->pos) {
						off++;
						continue;
					}
					if (!dir_emit(ctx, e->name, e->len,
						      0, DT_REG)) {
						err = 0;
						goto out;
					}
					off++;
					ctx->pos++;
				}
			}
		}
	}

	/* Phase 2: read base directory, skip overridden names */
	rdd.ctx.actor = agfs_fill_base;
	rdd.ctx.pos = 0;
	rdd.ns = ns;
	rdd.caller_ctx = ctx;
	rdd.off = &off;
	rdd.err = 0;

	lower_file->f_pos = 0;
	err = iterate_dir(lower_file, &rdd.ctx);
	if (!err && rdd.err)
		err = rdd.err;

out:
	agfs_nameset_free(ns);
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
