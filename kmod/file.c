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

/* Snapshot ino + snapshot_gen from the parent's dirent table. */
static void agfs_snapshot_de(struct dentry *dentry, u64 *ino, u64 *gen)
{
	struct inode *dir = d_inode(dentry->d_parent);
	struct agfs_inode_info *dii = AGFS_I(dir);
	struct agfs_dirent *de;

	spin_lock(&dii->de_lock);
	de = agfs_find_dirent(dir, dentry->d_name.name,
				 dentry->d_name.len);
	if (de) {
		*ino = de->ino;
		*gen = de->snapshot_gen;
	} else {
		*ino = 0;
		*gen = 0;
	}
	spin_unlock(&dii->de_lock);
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

	agfs_snapshot_de(dentry, &ino, &gen);

	if (!(file->f_flags & (O_WRONLY | O_RDWR)))
		return agfs_open_lower(dentry, file->f_flags);

	/* Fast path: inode is current — open directly */
	if (ino && gen >= (u64)atomic64_read(&sbi->snapshot_gen)) {
		down_read(&sbi->staging_sem);
		/* Re-check under lock — a snapshot may have raced */
		if (gen >= (u64)atomic64_read(&sbi->snapshot_gen)) {
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
	agfs_snapshot_de(dentry, &ino, &gen);
	if (ino && gen >= (u64)atomic64_read(&sbi->snapshot_gen)) {
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

/* ── open (§3.5 + §4.3) ───────────────────────────────────────────── */

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

/* ── Directory: readdir / iterate_shared (§3.8 merged listing) ──────── */

/*
 * Merged readdir: dirents first, then base entries not overridden.
 *
 * Stage dirent names are snapshotted into a hash set (for O(1) dedup), then
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
	unsigned char		d_type;
	char			name[];
};

static struct agfs_nameset *agfs_nameset_alloc(unsigned int hint)
{
	struct agfs_nameset *ns;
	unsigned int shift = 4;	/* minimum 16 buckets */

	while ((1u << shift) < hint * 2 && shift < 16)
		shift++;

	ns = kvzalloc(sizeof(*ns) + (sizeof(struct hlist_head) << shift),
		      GFP_KERNEL);
	if (!ns)
		return NULL;
	ns->shift = shift;
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
			    bool is_deleted, unsigned char d_type)
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
	e->d_type = d_type;
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

/*
 * Snapshot dirent entries into a nameset and emit non-deleted ones.
 * Returns the nameset (caller frees) or ERR_PTR on error.
 * Sets *done = true when dir_emit signals the buffer is full.
 */
static struct agfs_nameset *agfs_emit_dirents(struct inode *dir,
						struct dir_context *ctx,
						loff_t *off, bool *done)
{
	struct agfs_inode_info *dii = AGFS_I(dir);
	struct agfs_nameset *ns;
	struct agfs_dirent *de;
	struct agfs_nameset_entry *e;
	unsigned int count = 0, bi, i;
	int err;

	*done = false;

	if (!dii->de_buckets)
		return NULL;

	/* Count entries under spinlock */
	spin_lock(&dii->de_lock);
	for (bi = 0; bi < AGFS_DE_BUCKETS; bi++)
		hlist_for_each_entry(de, &dii->de_buckets[bi], node)
			count++;
	spin_unlock(&dii->de_lock);

	/* Allocate outside spinlock — GFP_KERNEL may sleep */
	ns = agfs_nameset_alloc(count);
	if (!ns)
		return ERR_PTR(-ENOMEM);

	/* Re-acquire and populate; table may have changed */
	spin_lock(&dii->de_lock);
	if (!dii->de_buckets) {
		spin_unlock(&dii->de_lock);
		return ns;
	}
	for (bi = 0; bi < AGFS_DE_BUCKETS; bi++) {
		hlist_for_each_entry(de, &dii->de_buckets[bi], node) {
			bool deleted = !de->ino && !de->base_path;

			err = agfs_nameset_add(ns, de->name, de->name_len,
					       deleted, de->d_type);
			if (err) {
				spin_unlock(&dii->de_lock);
				agfs_nameset_free(ns);
				return ERR_PTR(err);
			}
		}
	}
	spin_unlock(&dii->de_lock);

	/* Emit non-deleted dirents, respecting ctx->pos */
	for (i = 0; i < (1u << ns->shift); i++) {
		hlist_for_each_entry(e, &ns->buckets[i], node) {
			if (e->is_deleted) {
				(*off)++;
				continue;
			}
			if (*off < ctx->pos) {
				(*off)++;
				continue;
			}
			if (!dir_emit(ctx, e->name, e->len, 0, e->d_type)) {
				*done = true;
				return ns;
			}
			(*off)++;
			ctx->pos++;
		}
	}
	return ns;
}

/* ── readdir entry point ───────────────────────────────────────────── */

static int agfs_readdir(struct file *file, struct dir_context *ctx)
{
	struct agfs_file_info *fi = AGFS_F(file);
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);
	struct file *lower_file = fi->lower_file;
	struct agfs_nameset *ns;
	struct agfs_readdir_data rdd;
	loff_t off = 0;
	bool done;
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

	/* Phase 1: snapshot dirents, emit non-deleted entries */
	ns = agfs_emit_dirents(file_inode(file), ctx, &off, &done);
	if (IS_ERR(ns))
		return PTR_ERR(ns);
	if (done) {
		agfs_nameset_free(ns);
		return 0;
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
