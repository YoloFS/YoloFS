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
	struct agfs_dirent *old_de;
	struct path inode_path;
	unsigned char dt;
	bool in_base;
	u64 ino;
	int err;

	err = agfs_inode_alloc(sbi, &ino, &inode_path, mode, symname);
	if (err)
		return err;

	err = agfs_interpose(dentry, dir->i_sb, &inode_path);
	if (err) {
		path_put(&inode_path);
		return err;
	}

	agfs_replace_lower_path(dentry, &inode_path);
	dt = S_ISDIR(mode) ? DT_DIR : S_ISLNK(mode) ? DT_LNK : DT_REG;

	/* Check for tombstone dirent to inherit in_base. */
	old_de = agfs_find_dirent(dir, dentry->d_name.name,
				  dentry->d_name.len);
	in_base = old_de && agfs_pde_in_base(old_de->packed);

	err = agfs_add_dirent(dir, dentry->d_name.name,
			      dentry->d_name.len,
			      agfs_pde_inode(ino,
					    (u64)atomic64_read(&sbi->gen),
					    dt, in_base));
	if (err)
		return err;

	if (in_base)
		agfs_journal_modify(sbi, dentry, ino, dt);
	else
		agfs_journal_add(sbi, dentry, ino, dt);

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
	unsigned char d_type;
	int err;

	d_type = d_inode(dentry) ?
		 fs_umode_to_dtype(d_inode(dentry)->i_mode) : DT_UNKNOWN;

	err = agfs_del_dirent(dir,
				dentry->d_name.name,
				dentry->d_name.len);
	if (err)
		return err;

	err = agfs_journal_delete(sbi, dentry, d_type);
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

/* ── rename ────────────────────────────────────────────────────────── */

static int agfs_rename(struct mnt_idmap *idmap,
		       struct inode *old_dir, struct dentry *old_dentry,
		       struct inode *new_dir, struct dentry *new_dentry,
		       unsigned int flags)
{
	struct agfs_sb_info *sbi = AGFS_SB(old_dentry->d_sb);
	struct agfs_dirent *src_de, *dst_de;
	char old_buf[AGFS_PATH_MAX];
	unsigned char d_type = DT_UNKNOWN;
	agfs_pde_t packed;
	bool dst_in_base;
	int err;

	if (flags)
		return -EINVAL;

	/* old_buf is needed as the redirect base for base-only renames */
	err = agfs_dentry_relpath(old_dentry, old_buf, sizeof(old_buf));
	if (err)
		return err;

	/* VFS holds inode_lock(old_dir) + inode_lock(new_dir), which
	 * serializes dirent access.  staging_sem is not needed here —
	 * rename does not interact with gen or COW state. */

	/* Read source state */
	src_de = agfs_find_dirent(old_dir,
				  old_dentry->d_name.name,
				  old_dentry->d_name.len);
	if (src_de) {
		if (agfs_pde_is_tombstone(src_de->packed))
			return -ENOENT;
		d_type = agfs_pde_d_type(src_de->packed);
	} else if (d_inode(old_dentry)) {
		d_type = fs_umode_to_dtype(d_inode(old_dentry)->i_mode);
	}

	/* Check if destination has existing base content (for R vs P tag).
	 * Must be done before add_dirent replaces the dirent. */
	dst_de = agfs_find_dirent(new_dir,
				  new_dentry->d_name.name,
				  new_dentry->d_name.len);
	dst_in_base = dst_de ? agfs_pde_in_base(dst_de->packed) : false;
	if (!dst_de && d_inode(new_dentry))
		dst_in_base = true;

	/* Add destination dirent */
	if (src_de && agfs_pde_is_inode(src_de->packed)) {
		packed = agfs_pde_inode(agfs_pde_ino(src_de->packed),
				       agfs_pde_gen(src_de->packed),
				       d_type, dst_in_base);
	} else {
		packed = agfs_pde_link(old_buf, d_type, dst_in_base);
	}
	err = agfs_add_dirent(new_dir, new_dentry->d_name.name,
			      new_dentry->d_name.len, packed);
	if (err)
		goto out;

	/* Delete old name */
	err = agfs_del_dirent(old_dir,
			      old_dentry->d_name.name,
			      old_dentry->d_name.len);
	if (err)
		goto out;

	/* All renames emit a single R or P record.
	 * R if destination is new, P if destination existed in base. */
	if (dst_in_base)
		err = agfs_journal_replace(sbi, old_dentry, new_dentry, d_type);
	else
		err = agfs_journal_rename(sbi, old_dentry, new_dentry, d_type);

	/* Invalidate dcache for both names so next lookup uses dirents */
	d_drop(old_dentry);
	d_drop(new_dentry);

out:
	return err;
}

/* ── permission ────────────────────────────────────────────────────── */

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
	if (info->perm_gen != atomic64_read(&sbi->perm_gen)) {
		struct dentry *dentry = d_find_alias(inode);
		if (dentry) {
			agfs_cache_perm(inode, dentry);
			dput(dentry);
		}
	}
	perm = info->cached_perm;

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
