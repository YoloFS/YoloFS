// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — lookup and inode interposition.
 */

#include "agfs.h"

/* ── iget5 callbacks ───────────────────────────────────────────────── */

static int agfs_inode_test(struct inode *inode, void *data)
{
	struct inode *lower = data;
	return agfs_lower_inode(inode) == lower;
}

static int agfs_inode_set(struct inode *inode, void *data)
{
	struct inode *lower = data;

	agfs_set_lower_inode(inode, lower);
	fsstack_copy_attr_all(inode, lower);
	fsstack_copy_inode_size(inode, lower);
	inode->i_ino = lower->i_ino;
	set_nlink(inode, lower->i_nlink);
	return 0;
}

struct inode *agfs_iget(struct super_block *sb, struct inode *lower_inode)
{
	struct inode *inode;

	if (!lower_inode)
		return ERR_PTR(-ENOENT);

	inode = iget5_locked(sb, lower_inode->i_ino, agfs_inode_test,
			     agfs_inode_set, lower_inode);
	if (!inode)
		return ERR_PTR(-ENOMEM);

	if (!(inode->i_state & I_NEW))
		return inode;

	/* New inode — set up ops based on file type */
	ihold(lower_inode);
	agfs_set_lower_inode(inode, lower_inode);

	inode->i_mode = lower_inode->i_mode;
	inode_set_atime_to_ts(inode, inode_get_atime(lower_inode));
	inode_set_mtime_to_ts(inode, inode_get_mtime(lower_inode));
	inode_set_ctime_to_ts(inode, inode_get_ctime(lower_inode));

	if (S_ISDIR(lower_inode->i_mode)) {
		inode->i_op = &agfs_dir_iops;
		inode->i_fop = &agfs_dir_fops;
	} else if (S_ISLNK(lower_inode->i_mode)) {
		inode->i_op = &agfs_symlink_iops;
	} else if (S_ISREG(lower_inode->i_mode)) {
		inode->i_op = &agfs_main_iops;
		inode->i_fop = &agfs_main_fops;
		inode->i_mapping->a_ops = &agfs_aops;
	} else {
		/* block/char/fifo/socket — use lower ops directly */
		inode->i_op = &agfs_main_iops;
		init_special_inode(inode, lower_inode->i_mode,
				   lower_inode->i_rdev);
	}

	unlock_new_inode(inode);
	return inode;
}

/* ── Interposition ─────────────────────────────────────────────────── */

int agfs_interpose(struct dentry *dentry, struct super_block *sb,
		   struct path *lower_path)
{
	struct inode *inode;
	struct inode *lower_inode = d_inode(lower_path->dentry);

	inode = agfs_iget(sb, lower_inode);
	if (IS_ERR(inode))
		return PTR_ERR(inode);

	d_instantiate(dentry, inode);
	return 0;
}

/* ── Staging lookup helper ──────────────────────────────────────────── */

/*
 * Try to resolve a name through the staging dirent table.
 * Returns  1 if the name was resolved (dentry is fully set up),
 *          0 if staging did not handle it (fall through to base),
 *         <0 on error.
 */
static int agfs_lookup_staged(struct agfs_sb_info *sbi, struct dentry *dentry)
{
	struct inode *parent_inode = d_inode(dentry->d_parent);
	struct agfs_inode_info *parent_ii = AGFS_I(parent_inode);
	struct agfs_dirent *de;
	u64 ino = 0;
	char *bp = NULL;

	spin_lock(&parent_ii->de_lock);
	de = agfs_find_dirent(parent_inode,
				 dentry->d_name.name,
				 dentry->d_name.len);
	if (de) {
		ino = de->ino;
		if (de->base_path) {
			bp = kstrdup(de->base_path, GFP_ATOMIC);
			if (!bp) {
				spin_unlock(&parent_ii->de_lock);
				return -ENOMEM;
			}
		}
	}
	spin_unlock(&parent_ii->de_lock);

	if (!de)
		return 0;

	if (ino) {
		/* Staged inode */
		struct inode *inode;
		struct path ino_path;
		int err;

		kfree(bp);
		err = agfs_inode_path(sbi, ino, &ino_path);
		if (err)
			return 0; /* resolution failed — fall through to base */

		agfs_set_lower_path(dentry, &ino_path);
		inode = agfs_iget(dentry->d_sb, d_inode(ino_path.dentry));
		if (IS_ERR(inode)) {
			path_put(&ino_path);
			return PTR_ERR(inode);
		}
		agfs_cache_perm(inode, dentry);
		d_add(dentry, inode);
		return 1;
	}

	if (bp) {
		/* Redirected base path (zero-copy rename) */
		struct inode *inode;
		struct path base;
		int err;

		err = kern_path(bp, LOOKUP_FOLLOW, &base);
		kfree(bp);
		if (err)
			return 0; /* base path gone — fall through */

		agfs_set_lower_path(dentry, &base);
		inode = agfs_iget(dentry->d_sb, d_inode(base.dentry));
		if (IS_ERR(inode)) {
			path_put(&base);
			return PTR_ERR(inode);
		}
		agfs_cache_perm(inode, dentry);
		d_add(dentry, inode);
		return 1;
	}

	/* Deleted (ino=0, base_path=NULL) */
	d_add(dentry, NULL);
	return 1;
}

/* ── Lookup ────────────────────────────────────────────────────────── */

struct dentry *agfs_lookup(struct inode *dir, struct dentry *dentry,
			   unsigned int flags)
{
	struct agfs_sb_info *sbi = AGFS_SB(dir->i_sb);
	struct dentry *lower_dir_dentry;
	struct dentry *lower_dentry;
	struct vfsmount *lower_mnt;
	struct path lower_path;
	struct inode *inode;
	int err;

	err = agfs_new_dentry_private_data(dentry);
	if (err)
		return ERR_PTR(err);

	/* 1. Check staging dirent table */
	if (sbi->staging) {
		err = agfs_lookup_staged(sbi, dentry);
		if (err < 0)
			goto out_free;
		if (err > 0)
			return NULL;
	}

	/* 2. Fall back to base (lower) filesystem */
	lower_dir_dentry = agfs_lower_dentry(dentry->d_parent);
	lower_mnt = agfs_lower_mnt(dentry->d_parent);
	if (!lower_dir_dentry || !lower_mnt) {
		err = -ENOENT;
		goto out_free;
	}

	lower_dentry = lookup_one_len(dentry->d_name.name,
				      lower_dir_dentry,
				      dentry->d_name.len);
	if (IS_ERR(lower_dentry)) {
		err = PTR_ERR(lower_dentry);
		goto out_free;
	}

	lower_path.dentry = lower_dentry;
	lower_path.mnt = mntget(lower_mnt);
	agfs_set_lower_path(dentry, &lower_path);

	if (d_is_negative(lower_dentry)) {
		d_add(dentry, NULL);
		return NULL;
	}

	inode = agfs_iget(dentry->d_sb, d_inode(lower_dentry));
	if (IS_ERR(inode)) {
		err = PTR_ERR(inode);
		goto out_put;
	}

	agfs_cache_perm(inode, dentry);
	d_add(dentry, inode);
	return NULL;

out_put:
	path_put(&lower_path);
out_free:
	agfs_free_dentry_private_data(dentry);
	return ERR_PTR(err);
}
