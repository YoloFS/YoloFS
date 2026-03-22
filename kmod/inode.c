// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — inode operations.
 *
 * Directory operations (create, mkdir, unlink, rmdir, rename, symlink),
 * permission checking, setattr, getattr.
 */

#include "agfs.h"
#include <linux/xattr.h>

/* ── create/mkdir/symlink — allocate inode + set dstate on dentry ──── */

static int agfs_create_staged(struct inode *dir, struct dentry *dentry,
			      umode_t mode, const char *symname)
{
	struct agfs_sb_info *sbi = AGFS_SB(dir->i_sb);
	struct agfs_dentry_info *di = AGFS_D(dentry);
	struct path inode_path;
	unsigned char dt;
	bool already_staged, in_base;
	struct agfs_dstate dstate;
	u32 ino;
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

	/* If dentry is already on de_list, it's a tombstone — inherit in_base */
	already_staged = !list_empty(&di->de_node);
	in_base = already_staged;
	dstate = agfs_dstate_staged_inode(ino, (u16)atomic_read(&sbi->gen),
				dt, in_base);

	if (!already_staged)
		agfs_stage_dentry(dentry, dir, dstate);
	else
		di->dstate = dstate;

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

/* ── unlink/rmdir — tombstone or remove entry ────────────────────── */

static int agfs_delete_entry(struct inode *dir, struct dentry *dentry)
{
	struct agfs_sb_info *sbi = AGFS_SB(dentry->d_sb);
	struct agfs_dentry_info *di = AGFS_D(dentry);
	unsigned char d_type;
	bool in_base, need_tombstone;
	struct dentry *tomb = NULL;
	int err;

	d_type = d_inode(dentry) ?
		 fs_umode_to_dtype(d_inode(dentry)->i_mode) : DT_UNKNOWN;

	/* Determine whether we need a tombstone */
	if (!list_empty(&di->de_node)) {
		in_base = agfs_dstate_in_base(di->dstate);
		need_tombstone = in_base;
	} else {
		in_base = false;
		need_tombstone = true; /* base-only entry */
	}

	/* Pre-allocate tombstone before journaling so we can fail cleanly */
	if (need_tombstone) {
		tomb = agfs_add_tombstone(dentry->d_parent,
					  dentry->d_name.name,
					  dentry->d_name.len, dir,
					  d_type);
		if (!tomb)
			return -ENOMEM;
	}

	/* Journal (uses dentry path, must be before d_drop) */
	err = agfs_journal_delete(sbi, dentry, d_type);
	if (err) {
		if (tomb)
			agfs_remove_tombstone(tomb, dir);
		return err;
	}

	if (!list_empty(&di->de_node))
		agfs_unstage_dentry(di);

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
	struct agfs_dentry_info *old_di = AGFS_D(old_dentry);
	struct agfs_dentry_info *new_di = AGFS_D(new_dentry);
	char old_buf[AGFS_PATH_MAX];
	char saved_name[NAME_MAX + 1];
	unsigned int saved_name_len;
	struct dentry *saved_parent;
	struct dentry *tomb = NULL;
	unsigned char d_type = DT_UNKNOWN;
	struct agfs_dstate src_dstate, dst_dstate;
	char *base_copy = NULL;
	bool src_staged, dst_in_base, old_was_in_base;
	int err;

	if (flags)
		return -EINVAL;

	/* Save old name before d_move changes it */
	saved_name_len = old_dentry->d_name.len;
	memcpy(saved_name, old_dentry->d_name.name, saved_name_len);
	saved_name[saved_name_len] = '\0';
	saved_parent = old_dentry->d_parent;

	/* Relpath needed for base-only source (link redirect) */
	err = agfs_dentry_relpath(old_dentry, old_buf, sizeof(old_buf));
	if (err)
		return err;

	/* Read source state */
	src_staged = !list_empty(&old_di->de_node);
	src_dstate = src_staged ? old_di->dstate : (struct agfs_dstate){0};

	if (src_staged && !agfs_dstate_is_tombstone(src_dstate))
		d_type = agfs_dstate_d_type(src_dstate);
	else if (d_inode(old_dentry))
		d_type = fs_umode_to_dtype(d_inode(old_dentry)->i_mode);

	/* Check if destination has existing base content (for R vs P tag) */
	if (!list_empty(&new_di->de_node))
		dst_in_base = agfs_dstate_in_base(new_di->dstate);
	else
		dst_in_base = d_inode(new_dentry) != NULL;

	/* Determine if old name needs a tombstone */
	if (src_staged)
		old_was_in_base = agfs_dstate_in_base(src_dstate);
	else
		old_was_in_base = true; /* was only in base */

	/* Pre-allocate tombstone before any irreversible changes */
	if (old_was_in_base) {
		tomb = agfs_add_tombstone(saved_parent, saved_name,
					  saved_name_len, old_dir,
					  d_type);
		if (!tomb)
			return -ENOMEM;
	}

	/* Build destination dstate */
	if (src_staged && agfs_dstate_is_staged_inode(src_dstate)) {
		dst_dstate = agfs_dstate_staged_inode(agfs_dstate_ino(src_dstate),
					      agfs_dstate_gen(src_dstate),
					      d_type, dst_in_base);
	} else if (src_staged && agfs_dstate_is_base_path(src_dstate)) {
		base_copy = kstrdup(agfs_dstate_src(src_dstate), GFP_KERNEL);
		if (!base_copy) {
			err = -ENOMEM;
			goto out_tomb;
		}
		dst_dstate = agfs_dstate_base_path(base_copy, d_type, dst_in_base);
	} else {
		/* Base-only source — redirect via relpath */
		base_copy = kstrdup(old_buf, GFP_KERNEL);
		if (!base_copy) {
			err = -ENOMEM;
			goto out_tomb;
		}
		dst_dstate = agfs_dstate_base_path(base_copy, d_type, dst_in_base);
	}

	/* Journal BEFORE d_move (uses dentry paths) */
	if (dst_in_base)
		err = agfs_journal_replace(sbi, old_dentry, new_dentry, d_type);
	else
		err = agfs_journal_rename(sbi, old_dentry, new_dentry, d_type);
	if (err)
		goto out_free;

	/* Clean up new_dentry if it was staged */
	if (!list_empty(&new_di->de_node))
		agfs_unstage_dentry(new_di);

	/* Remove old_dentry from old parent's de_list */
	if (src_staged) {
		agfs_dstate_free(src_dstate);
		list_del_init(&old_di->de_node);
		dput(old_dentry);
	}

	/* Set destination dstate on old_dentry (will be at new position
	 * after d_move) and pin it on new parent's de_list */
	agfs_stage_dentry(old_dentry, new_dir, dst_dstate);

	/*
	 * d_drop old_dentry so d_move does not conflict with the
	 * tombstone dentry we may create at the old name.
	 */
	d_drop(old_dentry);

	/* d_drop new_dentry — VFS will call d_move(old_dentry, new_dentry)
	 * after we return, placing old_dentry at the new position. */
	d_drop(new_dentry);

	return 0;

out_free:
	kfree(base_copy);
out_tomb:
	if (tomb)
		agfs_remove_tombstone(tomb, old_dir);
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
