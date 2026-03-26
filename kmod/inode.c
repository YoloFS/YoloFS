// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — inode operations.
 *
 * Directory operations (create, mkdir, unlink, rmdir, rename, symlink),
 * permission checking, setattr, getattr.
 */

#include "agfs.h"
#include <linux/xattr.h>

/* ── create/mkdir/symlink — allocate inode + set up dentry ────────── */

static int agfs_create_staged(struct inode *dir, struct dentry *dentry,
			      umode_t mode, const char *symname)
{
	struct agfs_sb_info *sbi = AGFS_SB(dir->i_sb);
	struct path inode_path;
	u32 ino;
	int err;

	err = agfs_inode_alloc(sbi, &ino, &inode_path, mode, symname);
	if (err)
		return err;

	err = agfs_dentry_interpose(dentry, &inode_path);
	if (err)
		return err;

	agfs_dentry_pin(dentry, AGFS_TARGET_INODE);
	AGFS_I(d_inode(dentry))->staging_gen = (u16)atomic_read(&sbi->gen);

	agfs_journal_stage(sbi, dentry, ino);

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

/* ── unlink/rmdir — negative entry or remove entry ───────────────── */

static int agfs_delete_entry(struct inode *dir, struct dentry *dentry)
{
	struct agfs_sb_info *sbi = AGFS_SB(dentry->d_sb);
	struct dentry *tomb = NULL;
	int err;

	/* Pre-allocate negative dentry (tombstone) */
	tomb = agfs_dentry_create(dentry->d_parent,
				  dentry->d_name.name,
				  dentry->d_name.len,
				  AGFS_TARGET_NONE, NULL);
	if (IS_ERR(tomb))
		return PTR_ERR(tomb);

	/* Journal (uses dentry path, must be before d_drop) */
	err = agfs_journal_delete(sbi, dentry);
	if (err) {
		agfs_dentry_unpin(tomb);
		return err;
	}

	/* Release pinned state (if any) on the original dentry before eviction */
	agfs_dentry_unpin(dentry);
	d_drop(dentry);
	return 0;
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
	struct dentry *tomb = NULL;
	int err;

	if (flags)
		return -EINVAL;

	/*
	 * Always tombstone at old name, even if the source had no base
	 * content.  A spurious tombstone (hiding nothing in base) is
	 * harmless — lookup returns ENOENT, readdir skips it, and commit
	 * silently ignores a D for a non-existent base path.
	 */
	tomb = agfs_dentry_create(old_dentry->d_parent,
				  old_dentry->d_name.name,
				  old_dentry->d_name.len,
				  AGFS_TARGET_NONE, NULL);
	if (IS_ERR(tomb))
		return PTR_ERR(tomb);

	/* Journal BEFORE d_move (uses dentry paths) */
	err = agfs_journal_rename(sbi, old_dentry, new_dentry);
	if (err)
		goto out_tomb;

	/*
	 * Unhash both dentries before modifying pin state.
	 * VFS will call d_move(old_dentry, new_dentry) after we return,
	 * rehashing old_dentry at the new position.
	 */
	d_drop(old_dentry);
	d_drop(new_dentry);

	/* Release staging state on new_dentry (being replaced) */
	agfs_dentry_unpin(new_dentry);

	/* Pin old_dentry at its new position so it survives dcache pressure */
	agfs_dentry_pin(old_dentry, AGFS_D(old_dentry)->target);

	return 0;

out_tomb:
	agfs_dentry_unpin(tomb);
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
