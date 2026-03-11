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

/* ── create — create file in staging directory ─────────────────────── */

static int agfs_create(struct mnt_idmap *idmap, struct inode *dir,
		       struct dentry *dentry, umode_t mode, bool excl)
{
	struct agfs_sb_info *sbi = AGFS_SB(dir->i_sb);
	char buf[AGFS_PATH_MAX];
	struct path staging_parent_path;
	struct dentry *staging_dentry;
	struct inode *staging_dir;
	char *parent, *name, *tmp;
	int err;

	err = agfs_dentry_relpath(dentry, buf, sizeof(buf));
	if (err)
		return err;

	/* Create parent directories in staging */
	err = agfs_create_staging_parents(sbi, buf);
	if (err)
		return err;

	/* Split relpath into parent + name */
	tmp = kstrdup(buf, GFP_KERNEL);
	if (!tmp)
		return -ENOMEM;

	name = strrchr(tmp, '/');
	if (name) {
		*name = '\0';
		name++;
		parent = tmp;
	} else {
		name = tmp;
		parent = NULL;
	}

	/* Resolve parent dir in staging */
	if (parent && *parent)
		err = agfs_staging_path(sbi, parent, &staging_parent_path);
	else {
		staging_parent_path = sbi->staging_dir;
		path_get(&staging_parent_path);
		err = 0;
	}
	if (err) {
		kfree(tmp);
		return err;
	}

	staging_dir = d_inode(staging_parent_path.dentry);
	inode_lock_nested(staging_dir, I_MUTEX_PARENT);

	staging_dentry = lookup_one_len(name, staging_parent_path.dentry,
					strlen(name));
	if (IS_ERR(staging_dentry)) {
		err = PTR_ERR(staging_dentry);
		goto out_unlock;
	}

	err = vfs_create(mnt_idmap(staging_parent_path.mnt),
			 staging_dir, staging_dentry, mode, excl);
	if (err)
		goto out_dput;

	/* Interpose new inode */
	{
		struct path lower_path = {
			.dentry = staging_dentry,
			.mnt = staging_parent_path.mnt,
		};
		err = agfs_interpose(dentry, dir->i_sb, &lower_path);
	}
	if (err)
		goto out_dput;

	/* Update lower path on this dentry to point at staging */
	{
		struct path p = {
			.dentry = staging_dentry,
			.mnt = mntget(staging_parent_path.mnt),
		};
		agfs_set_lower_path(dentry, &p);
	}

	fsstack_copy_attr_times(dir, staging_dir);
	fsstack_copy_inode_size(dir, staging_dir);

	/* Success: staging_dentry ownership transferred */
	goto out_unlock;

out_dput:
	dput(staging_dentry);
out_unlock:
	inode_unlock(staging_dir);
	path_put(&staging_parent_path);
	kfree(tmp);
	return err;
}

/* ── mkdir — create directory in staging ────────────────────────────── */

static int agfs_mkdir(struct mnt_idmap *idmap, struct inode *dir,
		      struct dentry *dentry, umode_t mode)
{
	struct agfs_sb_info *sbi = AGFS_SB(dir->i_sb);
	char buf[AGFS_PATH_MAX];
	struct path staging_parent_path;
	struct dentry *staging_dentry;
	struct inode *staging_dir;
	char *parent, *name, *tmp;
	int err;

	err = agfs_dentry_relpath(dentry, buf, sizeof(buf));
	if (err)
		return err;

	err = agfs_create_staging_parents(sbi, buf);
	if (err)
		return err;

	tmp = kstrdup(buf, GFP_KERNEL);
	if (!tmp)
		return -ENOMEM;

	name = strrchr(tmp, '/');
	if (name) {
		*name = '\0';
		name++;
		parent = tmp;
	} else {
		name = tmp;
		parent = NULL;
	}

	if (parent && *parent)
		err = agfs_staging_path(sbi, parent, &staging_parent_path);
	else {
		staging_parent_path = sbi->staging_dir;
		path_get(&staging_parent_path);
		err = 0;
	}
	if (err) {
		kfree(tmp);
		return err;
	}

	staging_dir = d_inode(staging_parent_path.dentry);
	inode_lock_nested(staging_dir, I_MUTEX_PARENT);

	staging_dentry = lookup_one_len(name, staging_parent_path.dentry,
					strlen(name));
	if (IS_ERR(staging_dentry)) {
		err = PTR_ERR(staging_dentry);
		goto out_unlock;
	}

	err = vfs_mkdir(mnt_idmap(staging_parent_path.mnt),
			staging_dir, staging_dentry, mode);
	if (err)
		goto out_dput;

	{
		struct path lower_path = {
			.dentry = staging_dentry,
			.mnt = staging_parent_path.mnt,
		};
		err = agfs_interpose(dentry, dir->i_sb, &lower_path);
	}
	if (err)
		goto out_dput;

	{
		struct path p = {
			.dentry = staging_dentry,
			.mnt = mntget(staging_parent_path.mnt),
		};
		agfs_set_lower_path(dentry, &p);
	}

	fsstack_copy_attr_times(dir, staging_dir);
	fsstack_copy_inode_size(dir, staging_dir);
	set_nlink(dir, staging_dir->i_nlink);

	goto out_unlock;

out_dput:
	dput(staging_dentry);
out_unlock:
	inode_unlock(staging_dir);
	path_put(&staging_parent_path);
	kfree(tmp);
	return err;
}

/* ── unlink — create whiteout in staging ───────────────────────────── */

static int agfs_unlink(struct inode *dir, struct dentry *dentry)
{
	struct agfs_sb_info *sbi = AGFS_SB(dentry->d_sb);
	char buf[AGFS_PATH_MAX];
	int err;

	err = agfs_dentry_relpath(dentry, buf, sizeof(buf));
	if (err)
		return err;

	/* If a staging file exists, remove it first */
	if (agfs_staging_has(sbi, buf)) {
		struct path staging;
		err = agfs_staging_path(sbi, buf, &staging);
		if (!err && !agfs_is_whiteout(staging.dentry)) {
			struct dentry *parent = dget_parent(staging.dentry);
			inode_lock(d_inode(parent));
			err = vfs_unlink(mnt_idmap(staging.mnt),
					 d_inode(parent),
					 staging.dentry, NULL);
			inode_unlock(d_inode(parent));
			dput(parent);
			path_put(&staging);
			if (err)
				return err;
		} else if (!err) {
			path_put(&staging);
			/* Already a whiteout — nothing more to do */
			d_drop(dentry);
			return 0;
		}
	}

	/* Create whiteout */
	err = agfs_create_whiteout(sbi, buf);
	if (!err) {
		d_drop(dentry);
		fsstack_copy_attr_times(dir, d_inode(dentry->d_parent));
	}
	return err;
}

/* ── rmdir — create whiteout in staging ────────────────────────────── */

static int agfs_rmdir(struct inode *dir, struct dentry *dentry)
{
	struct agfs_sb_info *sbi = AGFS_SB(dentry->d_sb);
	char buf[AGFS_PATH_MAX];
	int err;

	err = agfs_dentry_relpath(dentry, buf, sizeof(buf));
	if (err)
		return err;

	/* Create whiteout (handles removing existing staging dir) */
	err = agfs_create_whiteout(sbi, buf);
	if (!err) {
		d_drop(dentry);
		fsstack_copy_attr_times(dir, d_inode(dentry->d_parent));
	}
	return err;
}

/* ── symlink ───────────────────────────────────────────────────────── */

static int agfs_symlink(struct mnt_idmap *idmap, struct inode *dir,
			struct dentry *dentry, const char *symname)
{
	struct agfs_sb_info *sbi = AGFS_SB(dir->i_sb);
	char buf[AGFS_PATH_MAX];
	struct path staging_parent_path;
	struct dentry *staging_dentry;
	struct inode *staging_dir;
	char *parent, *name, *tmp;
	int err;

	err = agfs_dentry_relpath(dentry, buf, sizeof(buf));
	if (err)
		return err;

	err = agfs_create_staging_parents(sbi, buf);
	if (err)
		return err;

	tmp = kstrdup(buf, GFP_KERNEL);
	if (!tmp)
		return -ENOMEM;

	name = strrchr(tmp, '/');
	if (name) {
		*name = '\0';
		name++;
		parent = tmp;
	} else {
		name = tmp;
		parent = NULL;
	}

	if (parent && *parent)
		err = agfs_staging_path(sbi, parent, &staging_parent_path);
	else {
		staging_parent_path = sbi->staging_dir;
		path_get(&staging_parent_path);
		err = 0;
	}
	if (err) {
		kfree(tmp);
		return err;
	}

	staging_dir = d_inode(staging_parent_path.dentry);
	inode_lock_nested(staging_dir, I_MUTEX_PARENT);

	staging_dentry = lookup_one_len(name, staging_parent_path.dentry,
					strlen(name));
	if (IS_ERR(staging_dentry)) {
		err = PTR_ERR(staging_dentry);
		goto out_unlock;
	}

	err = vfs_symlink(mnt_idmap(staging_parent_path.mnt),
			  staging_dir, staging_dentry, symname);
	if (err)
		goto out_dput;

	{
		struct path lower_path = {
			.dentry = staging_dentry,
			.mnt = staging_parent_path.mnt,
		};
		err = agfs_interpose(dentry, dir->i_sb, &lower_path);
	}
	if (err)
		goto out_dput;

	{
		struct path p = {
			.dentry = staging_dentry,
			.mnt = mntget(staging_parent_path.mnt),
		};
		agfs_set_lower_path(dentry, &p);
	}

	fsstack_copy_attr_times(dir, staging_dir);

	goto out_unlock;

out_dput:
	dput(staging_dentry);
out_unlock:
	inode_unlock(staging_dir);
	path_put(&staging_parent_path);
	kfree(tmp);
	return err;
}

/* ── rename (§3.5) ─────────────────────────────────────────────────── */

static int agfs_rename(struct mnt_idmap *idmap,
		       struct inode *old_dir, struct dentry *old_dentry,
		       struct inode *new_dir, struct dentry *new_dentry,
		       unsigned int flags)
{
	struct agfs_sb_info *sbi = AGFS_SB(old_dentry->d_sb);
	char old_buf[AGFS_PATH_MAX], new_buf[AGFS_PATH_MAX];
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

	if (agfs_staging_has(sbi, old_buf)) {
		/* Already staged — rename within staging dir */
		struct path old_staging, new_parent_path;
		struct dentry *new_staging;
		char *parent, *name, *buf2;

		err = agfs_staging_path(sbi, old_buf, &old_staging);
		if (err)
			goto out;

		/* Create parent dirs for destination */
		err = agfs_create_staging_parents(sbi, new_buf);
		if (err) {
			path_put(&old_staging);
			goto out;
		}

		/* Resolve new parent + name */
		buf2 = kstrdup(new_buf, GFP_KERNEL);
		if (!buf2) {
			path_put(&old_staging);
			err = -ENOMEM;
			goto out;
		}
		name = strrchr(buf2, '/');
		if (name) {
			*name = '\0';
			name++;
			parent = buf2;
		} else {
			name = buf2;
			parent = NULL;
		}

		if (parent && *parent)
			err = agfs_staging_path(sbi, parent, &new_parent_path);
		else {
			new_parent_path = sbi->staging_dir;
			path_get(&new_parent_path);
			err = 0;
		}
		if (err) {
			kfree(buf2);
			path_put(&old_staging);
			goto out;
		}

		{
			struct renamedata rd = {
				.old_mnt_idmap = mnt_idmap(old_staging.mnt),
				.old_dir = d_inode(old_staging.dentry->d_parent),
				.old_dentry = old_staging.dentry,
				.new_mnt_idmap = mnt_idmap(new_parent_path.mnt),
				.new_dir = d_inode(new_parent_path.dentry),
			};

			inode_lock(rd.old_dir);
			if (rd.old_dir != rd.new_dir)
				inode_lock_nested(rd.new_dir, I_MUTEX_CHILD);

			new_staging = lookup_one_len(name,
						     new_parent_path.dentry,
						     strlen(name));
			if (IS_ERR(new_staging)) {
				err = PTR_ERR(new_staging);
			} else {
				rd.new_dentry = new_staging;
				err = vfs_rename(&rd);
				dput(new_staging);
			}

			if (rd.old_dir != rd.new_dir)
				inode_unlock(rd.new_dir);
			inode_unlock(rd.old_dir);
		}

		kfree(buf2);
		path_put(&new_parent_path);
		path_put(&old_staging);
	} else {
		/* File only in base — redirect dentry + whiteout */
		struct agfs_dentry_info *old_di = AGFS_D(old_dentry);
		struct agfs_dentry_info *new_di = AGFS_D(new_dentry);

		if (new_di && old_di) {
			struct agfs_pinned_dentry *pd;

			pd = kzalloc(sizeof(*pd), GFP_KERNEL);
			if (!pd) {
				err = -ENOMEM;
				goto out;
			}

			spin_lock(&new_di->lock);
			new_di->lower_path = old_di->lower_path;
			path_get(&new_di->lower_path);
			spin_unlock(&new_di->lock);

			/* Pin dentry and track for cleanup on commit/abort/unmount */
			pd->dentry = dget(new_dentry);
			list_add(&pd->list, &sbi->pinned_dentries);

			/* Persist rename record for userspace */
			err = agfs_append_rename(sbi, old_buf, new_buf);
		}
	}

	/* Hide old path with whiteout — only on success */
	if (!err)
		err = agfs_create_whiteout(sbi, old_buf);

out:
	up_write(&sbi->staging_sem);
	if (!err) {
		fsstack_copy_attr_times(old_dir,
					d_inode(old_dentry->d_parent));
		fsstack_copy_attr_times(new_dir,
					d_inode(new_dentry->d_parent));
	}
	return err;
}

/* ── permission (§4.2) ─────────────────────────────────────────────── */

static int agfs_permission(struct mnt_idmap *idmap,
			   struct inode *inode, int mask)
{
	struct agfs_inode_info *info = AGFS_I(inode);
	struct agfs_sb_info *sbi = AGFS_SB(inode->i_sb);
	enum agfs_perm perm;

	/* Directories: delegate to lower FS */
	if (!S_ISREG(inode->i_mode))
		return inode_permission(idmap, agfs_lower_inode(inode), mask);

	if (sbi->noperm)
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

out:
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
