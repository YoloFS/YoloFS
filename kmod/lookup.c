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

/* ── Lookup ────────────────────────────────────────────────────────── */

struct dentry *agfs_lookup(struct inode *dir, struct dentry *dentry,
			   unsigned int flags)
{
	struct agfs_sb_info *sbi = AGFS_SB(dir->i_sb);
	struct dentry *lower_dir_dentry;
	struct dentry *lower_dentry;
	struct vfsmount *lower_mnt;
	struct path lower_path;
	struct inode *inode = NULL;
	int err;

	/* Allocate private data for this dentry */
	err = agfs_new_dentry_private_data(dentry);
	if (err)
		return ERR_PTR(err);

	/* 1. Check override list on parent directory inode */
	if (sbi->staging) {
		struct inode *parent_inode = d_inode(dentry->d_parent);
		struct agfs_inode_info *parent_ii = AGFS_I(parent_inode);
		struct agfs_override *ovr;
		u64 ino = 0;
		char *bp = NULL;

		spin_lock(&parent_ii->ovr_lock);
		ovr = agfs_find_override(parent_inode,
					 dentry->d_name.name,
					 dentry->d_name.len);
		if (ovr) {
			ino = ovr->ino;
			if (ovr->base_path) {
				bp = kstrdup(ovr->base_path, GFP_ATOMIC);
				if (!bp) {
					spin_unlock(&parent_ii->ovr_lock);
					err = -ENOMEM;
					goto out_free;
				}
			}
		}
		spin_unlock(&parent_ii->ovr_lock);

		if (ovr) {
			if (ino) {
				/* Staged inode */
				struct path ino_path;

				kfree(bp);
				err = agfs_inode_path(sbi, ino, &ino_path);

				if (!err) {
					agfs_set_lower_path(dentry, &ino_path);
					inode = agfs_iget(dentry->d_sb,
							  d_inode(ino_path.dentry));
					if (IS_ERR(inode)) {
						err = PTR_ERR(inode);
						path_put(&ino_path);
						goto out_free;
					}
					agfs_cache_perm(inode, dentry);
					d_add(dentry, inode);
					return NULL;
				}
				/* Inode resolution failed — fall through to base */
			} else if (bp) {
				/* Redirected base path (zero-copy rename) */
				struct path base;

				err = kern_path(bp, LOOKUP_FOLLOW, &base);
				kfree(bp);
				if (!err) {
					agfs_set_lower_path(dentry, &base);
					inode = agfs_iget(dentry->d_sb,
							  d_inode(base.dentry));
					if (IS_ERR(inode)) {
						err = PTR_ERR(inode);
						path_put(&base);
						goto out_free;
					}
					agfs_cache_perm(inode, dentry);
					d_add(dentry, inode);
					return NULL;
				}
				/* Base path gone — fall through */
			} else {
				/* Deleted (ino=0, base_path=NULL) */
				d_add(dentry, NULL);
				return NULL;
			}
		}
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

	/* Store the lower path */
	lower_path.dentry = lower_dentry;
	lower_path.mnt = mntget(lower_mnt);
	agfs_set_lower_path(dentry, &lower_path);

	/* If lower dentry is negative, return negative dentry */
	if (d_is_negative(lower_dentry)) {
		d_add(dentry, NULL);
		return NULL;
	}

	/* Interpose: create agfs inode for the lower inode */
	inode = agfs_iget(dentry->d_sb, d_inode(lower_dentry));
	if (IS_ERR(inode)) {
		err = PTR_ERR(inode);
		goto out_put;
	}

	/* Cache permission on the new inode */
	agfs_cache_perm(inode, dentry);

	d_add(dentry, inode);
	return NULL;

out_put:
	path_put(&lower_path);
out_free:
	agfs_free_dentry_private_data(dentry);
	return ERR_PTR(err);
}
