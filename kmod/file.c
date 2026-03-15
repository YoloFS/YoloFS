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

/* Open the right file for a staged regular file. */
static struct file *agfs_open_staged(struct agfs_sb_info *sbi,
				     struct dentry *dentry,
				     struct file *file, char *buf)
{
	struct agfs_dentry_info *parent_di = AGFS_D(dentry->d_parent);
	struct agfs_override *ovr;
	u64 sid = 0;
	int err;

	if (parent_di) {
		spin_lock(&parent_di->lock);
		ovr = agfs_find_override(dentry->d_parent,
					 dentry->d_name.name,
					 dentry->d_name.len);
		if (ovr)
			sid = ovr->staging_id;
		spin_unlock(&parent_di->lock);
	}

	if ((file->f_flags & (O_WRONLY | O_RDWR)) && sid) {
		struct path blob;
		struct file *f;

		err = agfs_staging_path(sbi, sid, &blob);
		if (err)
			return ERR_PTR(err);
		f = dentry_open(&blob, file->f_flags, current_cred());
		path_put(&blob);
		return f;
	}

	/* Not yet staged → open base; writable opens use O_RDONLY for COW */
	if (file->f_flags & (O_WRONLY | O_RDWR)) {
		file->f_flags &= ~O_TRUNC; /* defer truncation to COW */
		return agfs_open_lower(dentry, O_RDONLY);
	}
	return agfs_open_lower(dentry, file->f_flags);
}

/* ── open (§3.5 + §4.3) ───────────────────────────────────────────── */

static int agfs_open(struct inode *inode, struct file *file)
{
	struct agfs_sb_info *sbi = AGFS_SB(inode->i_sb);
	struct dentry *dentry = file->f_path.dentry;
	struct agfs_file_info *fi;
	const struct cred *old_cred;
	struct file *lower_file;
	unsigned int orig_flags = file->f_flags;
	char buf[AGFS_PATH_MAX];
	int err;

	fi = kzalloc(sizeof(*fi), GFP_KERNEL);
	if (!fi)
		return -ENOMEM;

	if (S_ISREG(inode->i_mode) && !sbi->noperm) {
		err = agfs_check_open_perm(sbi, dentry, file, buf);
		if (err)
			goto out_free;
	}

	if (S_ISREG(inode->i_mode) && !sbi->nostaging) {
		old_cred = override_creds(sbi->creator_cred);
		lower_file = agfs_open_staged(sbi, dentry, file, buf);
		revert_creds(old_cred);
	} else {
		lower_file = agfs_open_lower(dentry, file->f_flags);
	}

	if (IS_ERR(lower_file)) {
		err = PTR_ERR(lower_file);
		goto out_free;
	}

	/* O_TRUNC on unstaged file: agfs_open_staged stripped it from f_flags;
	 * defer actual truncation to first write via agfs_cow_if_needed. */
	fi->truncate = (orig_flags & O_TRUNC) && !(file->f_flags & O_TRUNC);
	fi->lower_file = lower_file;
	file->private_data = fi;
	return 0;

out_free:
	kfree(fi);
	return err;
}

/*
 * Trigger COW / re-COW if the inode's snapshot_gen is behind the
 * superblock's, or if deferred truncation is pending.
 * Returns 0 on success (or if no COW was needed).
 * Caller must NOT hold staging_sem.
 */
static int agfs_cow_if_needed(struct file *file)
{
	struct agfs_file_info *fi = AGFS_F(file);
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);
	struct agfs_inode_info *ii = AGFS_I(file_inode(file));
	const struct cred *old_cred;
	struct file *new_file = NULL;
	bool truncate = fi->truncate;
	int err;

	if (!truncate &&
	    ii->snapshot_gen >= (u64)atomic64_read(&sbi->snapshot_gen))
		return 0;

	old_cred = override_creds(sbi->creator_cred);
	down_write(&sbi->staging_sem);
	if (truncate ||
	    ii->snapshot_gen <
	    (u64)atomic64_read(&sbi->snapshot_gen)) {
		err = agfs_do_cow(sbi, file->f_path.dentry,
				       &new_file,
				       file->f_flags & ~O_TRUNC, truncate);
		if (err) {
			up_write(&sbi->staging_sem);
			revert_creds(old_cred);
			return err;
		}
		fput(fi->lower_file);
		fi->lower_file = new_file;
		fi->truncate = false;
		/* inode->snapshot_gen updated inside agfs_do_cow */
	}
	up_write(&sbi->staging_sem);
	revert_creds(old_cred);
	return 0;
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
	struct file *lower_file;
	ssize_t ret;
	int err;

	err = agfs_cow_if_needed(file);
	if (err)
		return err;

	/* Write to staging blob */
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
	struct file *lower_file;
	int err;

	/*
	 * Writable shared mapping needs a writable lower file.
	 * Trigger COW / re-COW only if the file was opened for writing.
	 */
	if ((file->f_flags & (O_WRONLY | O_RDWR)) &&
	    (vma->vm_flags & (VM_WRITE | VM_SHARED)) ==
	    (VM_WRITE | VM_SHARED)) {
		err = agfs_cow_if_needed(file);
		if (err)
			return err;
	}

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

	/* COW'd files live in staging and are ephemeral — skip fsync */
	if (AGFS_I(file_inode(file))->snapshot_gen > 0)
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
	struct agfs_nameset_entry *e;
	unsigned int idx;

	if (!ns)
		return false;
	idx = agfs_name_hash(name, len, ns->shift);

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
	struct agfs_nameset_entry *e;
	unsigned int idx;

	if (!ns)
		return -EINVAL;
	idx = agfs_name_hash(name, len, ns->shift);

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
	if (di && di->ovr_buckets) {
		struct agfs_override *ovr;
		unsigned int count = 0;
		unsigned int bi;

		spin_lock(&di->lock);
		for (bi = 0; bi < AGFS_OVR_BUCKETS; bi++)
			hlist_for_each_entry(ovr, &di->ovr_buckets[bi], node)
				count++;

		ns = agfs_nameset_alloc(count);
		if (!ns) {
			spin_unlock(&di->lock);
			return -ENOMEM;
		}

		for (bi = 0; bi < AGFS_OVR_BUCKETS; bi++) {
			hlist_for_each_entry(ovr, &di->ovr_buckets[bi], node) {
				bool deleted = !ovr->staging_id && !ovr->base_path;

				err = agfs_nameset_add(ns, ovr->name,
						       ovr->name_len, deleted);
				if (err) {
					spin_unlock(&di->lock);
					goto out;
				}
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
