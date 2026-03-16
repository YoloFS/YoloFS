// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — inode operations.
 *
 * Directory operations (create, mkdir, unlink, rmdir, rename, symlink),
 * permission checking, setattr, getattr.
 */

#include "agfs.h"
#include <linux/xattr.h>

/* ── create/mkdir/symlink — allocate inode + dirent ───────────────── */

static int agfs_create_staged(struct inode *dir, struct dentry *dentry,
			      umode_t mode, const char *symname)
{
	struct agfs_sb_info *sbi = AGFS_SB(dir->i_sb);
	char buf[AGFS_PATH_MAX];
	struct path inode_path;
	u64 ino;
	int err;

	err = agfs_dentry_relpath(dentry, buf, sizeof(buf));
	if (err)
		return err;

	err = agfs_inode_alloc(sbi, &ino, &inode_path, mode, symname);
	if (err)
		return err;

	err = agfs_interpose(dentry, dir->i_sb, &inode_path);
	if (err) {
		path_put(&inode_path);
		return err;
	}

	agfs_replace_lower_path(dentry, &inode_path);
	err = agfs_add_dirent(dir, dentry->d_name.name,
				dentry->d_name.len, ino, NULL,
				S_ISDIR(mode) ? DT_DIR :
				S_ISLNK(mode) ? DT_LNK : DT_REG,
				(u64)atomic64_read(&sbi->snapshot_gen));
	if (err)
		return err;
	agfs_journal_append_a(sbi, buf, ino);

	return 0;
}

static int agfs_create(struct mnt_idmap *idmap, struct inode *dir,
		       struct dentry *dentry, umode_t mode, bool excl)
{
	return agfs_create_staged(dir, dentry, mode, NULL);
}

static int agfs_mkdir(struct mnt_idmap *idmap, struct inode *dir,
		      struct dentry *dentry, umode_t mode)
{
	return agfs_create_staged(dir, dentry, S_IFDIR | mode, NULL);
}

/* ── unlink/rmdir — add DELETED dirent ───────────────────────────── */

static int agfs_delete_entry(struct inode *dir, struct dentry *dentry)
{
	struct agfs_sb_info *sbi = AGFS_SB(dentry->d_sb);
	char buf[AGFS_PATH_MAX];
	int err;

	err = agfs_dentry_relpath(dentry, buf, sizeof(buf));
	if (err)
		return err;

	err = agfs_add_dirent(dir,
				dentry->d_name.name,
				dentry->d_name.len, 0, NULL, DT_UNKNOWN,
				0);
	if (err)
		return err;

	err = agfs_journal_append_d(sbi, buf);
	if (!err)
		d_drop(dentry);
	return err;
}

static int agfs_unlink(struct inode *dir, struct dentry *dentry)
{
	return agfs_delete_entry(dir, dentry);
}

static int agfs_rmdir(struct inode *dir, struct dentry *dentry)
{
	return agfs_delete_entry(dir, dentry);
}

/* ── symlink ───────────────────────────────────────────────────────── */

static int agfs_symlink(struct mnt_idmap *idmap, struct inode *dir,
			struct dentry *dentry, const char *symname)
{
	return agfs_create_staged(dir, dentry, S_IFLNK, symname);
}

/* ── rename (§3.7) ─────────────────────────────────────────────────── */

static int agfs_rename(struct mnt_idmap *idmap,
		       struct inode *old_dir, struct dentry *old_dentry,
		       struct inode *new_dir, struct dentry *new_dentry,
		       unsigned int flags)
{
	struct agfs_sb_info *sbi = AGFS_SB(old_dentry->d_sb);
	struct agfs_inode_info *old_parent_ii;
	char old_buf[AGFS_PATH_MAX], new_buf[AGFS_PATH_MAX];
	struct agfs_dirent *old_de = NULL;
	u64 old_ino = 0;
	u64 old_gen = 0;
	char *old_bp = NULL;
	unsigned char old_dtype = DT_UNKNOWN;
	int err;

	if (flags)
		return -EINVAL;

	err = agfs_dentry_relpath(old_dentry, old_buf, sizeof(old_buf));
	if (err)
		return err;
	err = agfs_dentry_relpath(new_dentry, new_buf, sizeof(new_buf));
	if (err)
		return err;

	down_write(&sbi->staging_sem);

	/* Check current dirent state on old name.
	 * Snapshot base_path while holding the lock. */
	old_parent_ii = AGFS_I(old_dir);
	spin_lock(&old_parent_ii->de_lock);
	old_de = agfs_find_dirent(old_dir,
				     old_dentry->d_name.name,
				     old_dentry->d_name.len);
	if (old_de) {
		old_ino = old_de->ino;
		old_gen = old_de->snapshot_gen;
		old_dtype = old_de->d_type;
		if (old_de->base_path) {
			old_bp = kstrdup(old_de->base_path,
					 GFP_ATOMIC);
			if (!old_bp) {
				spin_unlock(&old_parent_ii->de_lock);
				err = -ENOMEM;
				goto out;
			}
		}
	}
	spin_unlock(&old_parent_ii->de_lock);

	/* Derive d_type from old dentry's inode when no dirent existed */
	if (!old_de && d_inode(old_dentry))
		old_dtype = fs_umode_to_dtype(d_inode(old_dentry)->i_mode);

	if (old_de && !old_ino && !old_bp) {
		/* Source is deleted — cannot rename */
		err = -ENOENT;
		goto out;
	} else if (old_ino) {
		/* File has a staged inode — move the dirent, keep same ino */
		err = agfs_add_dirent(new_dir,
					new_dentry->d_name.name,
					new_dentry->d_name.len,
					old_ino, NULL, old_dtype,
					old_gen);
	} else {
		/* Base file or chained rename — redirect by path */
		err = agfs_add_dirent(new_dir,
					new_dentry->d_name.name,
					new_dentry->d_name.len,
					0, old_bp ? old_bp : old_buf,
					old_dtype, 0);
	}
	if (err)
		goto out;

	/* Hide the old name (deleted dirent) */
	err = agfs_add_dirent(old_dir,
				old_dentry->d_name.name,
				old_dentry->d_name.len, 0, NULL,
				DT_UNKNOWN, 0);
	if (err)
		goto out;

	/* Journal rename */
	err = agfs_journal_append_r(sbi, old_buf, new_buf);

	/* Invalidate dcache for both names so next lookup uses dirents */
	d_drop(old_dentry);
	d_drop(new_dentry);

out:
	kfree(old_bp);
	up_write(&sbi->staging_sem);
	return err;
}

/* ── permission (§4.2) ─────────────────────────────────────────────── */

static int agfs_permission(struct mnt_idmap *idmap,
			   struct inode *inode, int mask)
{
	struct agfs_inode_info *info = AGFS_I(inode);
	struct agfs_sb_info *sbi = AGFS_SB(inode->i_sb);
	enum agfs_perm perm;

	/* Skip all permission gating if disabled */
	if (!sbi->permission)
		return 0;

	/* Directories: delegate to lower FS */
	if (!S_ISREG(inode->i_mode))
		return inode_permission(idmap, agfs_lower_inode(inode), mask);

	/* Check generation — re-resolve if stale */
	perm = info->cached_perm;
	if (info->perm_gen != atomic64_read(&sbi->perm_gen)) {
		struct dentry *dentry = d_find_alias(inode);
		if (dentry) {
			perm = agfs_resolve_perm(dentry);
			info->cached_perm = perm;
			info->perm_gen = atomic64_read(&sbi->perm_gen);
			dput(dentry);
		}
	}

	/* Ask is handled in open(), not here */
	if (perm == AGFS_PERM_ASK)
		return 0;

	switch (perm) {
	case AGFS_PERM_ALLOW:
		return 0;
	case AGFS_PERM_ALLOW_RW:
		return (mask & MAY_EXEC) ? -EACCES : 0;
	case AGFS_PERM_ALLOW_RO:
		return (mask & (MAY_WRITE | MAY_EXEC)) ? -EACCES : 0;
	case AGFS_PERM_ALLOW_RX:
		return (mask & MAY_WRITE) ? -EACCES : 0;
	case AGFS_PERM_DENY:
		return -EACCES;
	default:
		return -EACCES;
	}
}

/* ── setattr ───────────────────────────────────────────────────────── */

static int agfs_setattr(struct mnt_idmap *idmap,
			struct dentry *dentry, struct iattr *ia)
{
	struct agfs_sb_info *sbi = AGFS_SB(dentry->d_sb);
	struct path lower_path;
	struct inode *inode = d_inode(dentry);
	struct inode *lower_inode;
	int err;

	err = setattr_prepare(idmap, dentry, ia);
	if (err)
		return err;

	/* If truncating, sync upper inode size */
	if (ia->ia_valid & ATTR_SIZE) {
		err = inode_newsize_ok(inode, ia->ia_size);
		if (err)
			return err;
		truncate_setsize(inode, ia->ia_size);

		/* When staging is active, don't propagate size changes
		 * to the base file — the staging copy is the data store.
		 * The VFS triggers this via O_TRUNC after open; the
		 * staging file is already the correct size. */
		if (sbi->staging && S_ISREG(inode->i_mode))
			ia->ia_valid &= ~ATTR_SIZE;
	}

	/* If nothing left to propagate, we're done */
	ia->ia_valid &= ~ATTR_MODE;
	if (!ia->ia_valid)
		return 0;

	agfs_get_lower_path(dentry, &lower_path);
	lower_inode = d_inode(lower_path.dentry);

	inode_lock(lower_inode);
	err = notify_change(mnt_idmap(lower_path.mnt), lower_path.dentry,
			    ia, NULL);
	inode_unlock(lower_inode);

	if (!err) {
		fsstack_copy_attr_all(inode, lower_inode);
		fsstack_copy_inode_size(inode, lower_inode);
	}

	agfs_put_lower_path(dentry, &lower_path);
	return err;
}

/* ── getattr ───────────────────────────────────────────────────────── */

static int agfs_getattr(struct mnt_idmap *idmap,
			const struct path *path, struct kstat *stat,
			u32 request_mask, unsigned int query_flags)
{
	struct dentry *dentry = path->dentry;
	struct inode *inode = d_inode(dentry);
	struct path lower_path;
	struct inode *lower_inode;
	int err;

	agfs_get_lower_path(dentry, &lower_path);
	lower_inode = d_inode(lower_path.dentry);
	err = vfs_getattr_nosec(&lower_path, stat, request_mask, query_flags);
	if (!err)
		fsstack_copy_attr_all(inode, lower_inode);
	agfs_put_lower_path(dentry, &lower_path);

	if (!err)
		stat->dev = dentry->d_sb->s_dev;
	return err;
}

/* ── listxattr ─────────────────────────────────────────────────────── */

static ssize_t agfs_listxattr(struct dentry *dentry, char *buffer,
			      size_t buffer_size)
{
	struct path lower_path;
	ssize_t err;

	agfs_get_lower_path(dentry, &lower_path);
	err = vfs_listxattr(lower_path.dentry, buffer, buffer_size);
	agfs_put_lower_path(dentry, &lower_path);
	return err;
}

/* ── symlink iops ──────────────────────────────────────────────────── */

static const char *agfs_get_link(struct dentry *dentry, struct inode *inode,
				 struct delayed_call *done)
{
	const char *link;
	struct dentry *lower_dentry;

	if (!dentry)
		return ERR_PTR(-ECHILD);

	lower_dentry = agfs_lower_dentry(dentry);
	if (!lower_dentry || !d_inode(lower_dentry) ||
	    !d_inode(lower_dentry)->i_op->get_link)
		return ERR_PTR(-ENOENT);

	link = d_inode(lower_dentry)->i_op->get_link(lower_dentry,
						     d_inode(lower_dentry),
						     done);
	return link;
}

/* ── Ops Tables ────────────────────────────────────────────────────── */

const struct inode_operations agfs_dir_iops = {
	.lookup		= agfs_lookup,
	.create		= agfs_create,
	.mkdir		= agfs_mkdir,
	.unlink		= agfs_unlink,
	.rmdir		= agfs_rmdir,
	.symlink	= agfs_symlink,
	.rename		= agfs_rename,
	.permission	= agfs_permission,
	.setattr	= agfs_setattr,
	.getattr	= agfs_getattr,
	.listxattr	= agfs_listxattr,
};

const struct inode_operations agfs_main_iops = {
	.permission	= agfs_permission,
	.setattr	= agfs_setattr,
	.getattr	= agfs_getattr,
	.listxattr	= agfs_listxattr,
};

const struct inode_operations agfs_symlink_iops = {
	.get_link	= agfs_get_link,
	.permission	= agfs_permission,
	.setattr	= agfs_setattr,
	.getattr	= agfs_getattr,
	.listxattr	= agfs_listxattr,
};
