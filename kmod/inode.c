// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — inode operations.
 *
 * Directory operations (create, mkdir, unlink, rmdir, rename, symlink),
 * permission checking, setattr, getattr.
 */

#include "agfs.h"
#include <linux/xattr.h>
#include <linux/mm.h>

/* ── create — allocate staging blob + override ─────────────────────── */

static int agfs_create(struct mnt_idmap *idmap, struct inode *dir,
		       struct dentry *dentry, umode_t mode, bool excl)
{
	struct agfs_sb_info *sbi = AGFS_SB(dir->i_sb);
	const struct cred *old_cred;
	char buf[AGFS_PATH_MAX];
	struct path blob_path;
	u64 id;
	int err;

	err = agfs_dentry_relpath(dentry, buf, sizeof(buf));
	if (err)
		return err;

	old_cred = override_creds(sbi->creator_cred);

	err = agfs_staging_alloc(sbi, &id, &blob_path, mode, NULL);
	if (err)
		goto out_revert;

	err = agfs_interpose(dentry, dir->i_sb, &blob_path);
	if (err) {
		path_put(&blob_path);
		goto out_revert;
	}

	agfs_set_lower_path(dentry, &blob_path);
	agfs_add_override(dentry->d_parent, dentry->d_name.name,
			  dentry->d_name.len, id, NULL);
	agfs_journal_append_a(sbi, buf, id);

	revert_creds(old_cred);
	return 0;

out_revert:
	revert_creds(old_cred);
	return err;
}

static int agfs_mkdir(struct mnt_idmap *idmap, struct inode *dir,
		      struct dentry *dentry, umode_t mode)
{
	struct agfs_sb_info *sbi = AGFS_SB(dir->i_sb);
	const struct cred *old_cred;
	char buf[AGFS_PATH_MAX];
	struct path blob_path;
	u64 id;
	int err;

	err = agfs_dentry_relpath(dentry, buf, sizeof(buf));
	if (err)
		return err;

	old_cred = override_creds(sbi->creator_cred);

	err = agfs_staging_alloc(sbi, &id, &blob_path, S_IFDIR | mode, NULL);
	if (err)
		goto out_revert;

	err = agfs_interpose(dentry, dir->i_sb, &blob_path);
	if (err) {
		path_put(&blob_path);
		goto out_revert;
	}

	agfs_set_lower_path(dentry, &blob_path);
	agfs_add_override(dentry->d_parent, dentry->d_name.name,
			  dentry->d_name.len, id, NULL);
	agfs_journal_append_a(sbi, buf, id);

	revert_creds(old_cred);
	return 0;

out_revert:
	revert_creds(old_cred);
	return err;
}

/* ── unlink — add DELETED override ──────────────────────────────────── */

static int agfs_unlink(struct inode *dir, struct dentry *dentry)
{
	struct agfs_sb_info *sbi = AGFS_SB(dentry->d_sb);
	const struct cred *old_cred;
	char buf[AGFS_PATH_MAX];
	int err;

	err = agfs_dentry_relpath(dentry, buf, sizeof(buf));
	if (err)
		return err;

	old_cred = override_creds(sbi->creator_cred);

	/* Add deleted override (staging_id=0, base_path=NULL) */
	err = agfs_add_override(dentry->d_parent,
				dentry->d_name.name,
				dentry->d_name.len, 0, NULL);
	if (err)
		goto out;

	/* Append journal record */
	err = agfs_journal_append_d(sbi, buf);
	if (!err)
		d_drop(dentry);
out:
	revert_creds(old_cred);
	return err;
}

/* ── rmdir — add DELETED override ──────────────────────────────────── */

static int agfs_rmdir(struct inode *dir, struct dentry *dentry)
{
	struct agfs_sb_info *sbi = AGFS_SB(dentry->d_sb);
	const struct cred *old_cred;
	char buf[AGFS_PATH_MAX];
	int err;

	err = agfs_dentry_relpath(dentry, buf, sizeof(buf));
	if (err)
		return err;

	old_cred = override_creds(sbi->creator_cred);

	err = agfs_add_override(dentry->d_parent,
				dentry->d_name.name,
				dentry->d_name.len, 0, NULL);
	if (err)
		goto out;

	err = agfs_journal_append_d(sbi, buf);
	if (!err)
		d_drop(dentry);
out:
	revert_creds(old_cred);
	return err;
}

/* ── symlink ───────────────────────────────────────────────────────── */

static int agfs_symlink(struct mnt_idmap *idmap, struct inode *dir,
			struct dentry *dentry, const char *symname)
{
	struct agfs_sb_info *sbi = AGFS_SB(dir->i_sb);
	const struct cred *old_cred;
	char buf[AGFS_PATH_MAX];
	struct path blob_path;
	u64 id;
	int err;

	err = agfs_dentry_relpath(dentry, buf, sizeof(buf));
	if (err)
		return err;

	old_cred = override_creds(sbi->creator_cred);

	err = agfs_staging_alloc(sbi, &id, &blob_path, S_IFLNK, symname);
	if (err)
		goto out_revert;

	err = agfs_interpose(dentry, dir->i_sb, &blob_path);
	if (err) {
		path_put(&blob_path);
		goto out_revert;
	}

	agfs_set_lower_path(dentry, &blob_path);
	agfs_add_override(dentry->d_parent, dentry->d_name.name,
			  dentry->d_name.len, id, NULL);
	agfs_journal_append_a(sbi, buf, id);

	revert_creds(old_cred);
	return 0;

out_revert:
	revert_creds(old_cred);
	return err;
}

/* ── rename (§3.7) ─────────────────────────────────────────────────── */

static int agfs_rename(struct mnt_idmap *idmap,
		       struct inode *old_dir, struct dentry *old_dentry,
		       struct inode *new_dir, struct dentry *new_dentry,
		       unsigned int flags)
{
	struct agfs_sb_info *sbi = AGFS_SB(old_dentry->d_sb);
	struct agfs_dentry_info *old_parent_di;
	const struct cred *old_cred;
	char old_buf[AGFS_PATH_MAX], new_buf[AGFS_PATH_MAX];
	struct agfs_override *old_ovr = NULL;
	u64 old_sid = 0;
	char *old_bp = NULL;
	int err;

	if (flags)
		return -EINVAL;

	err = agfs_dentry_relpath(old_dentry, old_buf, sizeof(old_buf));
	if (err)
		return err;
	err = agfs_dentry_relpath(new_dentry, new_buf, sizeof(new_buf));
	if (err)
		return err;

	old_cred = override_creds(sbi->creator_cred);
	down_write(&sbi->staging_sem);

	/* Check current override state on old name.
	 * Snapshot base_path while holding the lock (§3.4). */
	old_parent_di = AGFS_D(old_dentry->d_parent);
	if (old_parent_di) {
		spin_lock(&old_parent_di->lock);
		old_ovr = agfs_find_override(old_dentry->d_parent,
					     old_dentry->d_name.name,
					     old_dentry->d_name.len);
		if (old_ovr) {
			old_sid = old_ovr->staging_id;
			if (old_ovr->base_path) {
				old_bp = kstrdup(old_ovr->base_path,
						 GFP_ATOMIC);
				if (!old_bp) {
					spin_unlock(&old_parent_di->lock);
					err = -ENOMEM;
					goto out;
				}
			}
		}
		spin_unlock(&old_parent_di->lock);
	}

	if (old_ovr && !old_sid && !old_bp) {
		/* Source is deleted — cannot rename */
		err = -ENOENT;
		goto out;
	} else if (old_sid) {
		/* File is in a staging blob — move the override */
		err = agfs_add_override(new_dentry->d_parent,
					new_dentry->d_name.name,
					new_dentry->d_name.len,
					old_sid, NULL);
	} else if (old_bp) {
		/* Already redirected (chained rename) — follow the chain */
		err = agfs_add_override(new_dentry->d_parent,
					new_dentry->d_name.name,
					new_dentry->d_name.len,
					0, old_bp);
	} else {
		/* File only in base — redirect without copying */
		err = agfs_add_override(new_dentry->d_parent,
					new_dentry->d_name.name,
					new_dentry->d_name.len,
					0, old_buf);
	}
	if (err)
		goto out;

	/* Hide the old name (deleted override) */
	err = agfs_add_override(old_dentry->d_parent,
				old_dentry->d_name.name,
				old_dentry->d_name.len, 0, NULL);
	if (err)
		goto out;

	/* Journal rename */
	err = agfs_journal_append_r(sbi, old_buf, new_buf);

	/* Invalidate dcache for both names so next lookup uses overrides */
	d_drop(old_dentry);
	d_drop(new_dentry);

out:
	kfree(old_bp);
	up_write(&sbi->staging_sem);
	revert_creds(old_cred);
	return err;
}

/* ── permission (§4.2) ─────────────────────────────────────────────── */

static int agfs_permission(struct mnt_idmap *idmap,
			   struct inode *inode, int mask)
{
	struct agfs_inode_info *info = AGFS_I(inode);
	struct agfs_sb_info *sbi = AGFS_SB(inode->i_sb);
	enum agfs_perm perm;

	/* noperm: skip all permission gating (including staging-owned dirs) */
	if (sbi->noperm)
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
		if (!sbi->nostaging && S_ISREG(inode->i_mode))
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
