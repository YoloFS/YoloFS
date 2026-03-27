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

/* ── Lookup ────────────────────────────────────────────────────────── */

/*
 * All pinned entries are held in the dcache via dget(), so
 * lookup_fast() finds them directly — this callback is only invoked
 * for unpinned names.  Fall through to the base filesystem.
 */
struct dentry *agfs_lookup(struct inode *dir, struct dentry *dentry,
			   unsigned int flags)
{
	struct dentry *lower_dir_dentry;
	struct dentry *lower_dentry;
	struct vfsmount *lower_mnt;
	struct path lower_path;
	int err;

	/* d_init already allocated d_fsdata */

	/* Base (lower) filesystem lookup */
	lower_dir_dentry = agfs_lower_dentry(dentry->d_parent);
	lower_mnt = agfs_lower_mnt(dentry->d_parent);
	if (!lower_dir_dentry || !lower_mnt)
		return ERR_PTR(-ENOENT);

	inode_lock_shared(d_inode(lower_dir_dentry));
	lower_dentry = lookup_one_len(dentry->d_name.name,
				      lower_dir_dentry,
				      dentry->d_name.len);
	inode_unlock_shared(d_inode(lower_dir_dentry));
	if (IS_ERR(lower_dentry))
		return ERR_CAST(lower_dentry);

	lower_path.dentry = lower_dentry;
	lower_path.mnt = mntget(lower_mnt);

	err = agfs_dentry_interpose(dentry, &lower_path);
	if (err)
		return ERR_PTR(err);

	if (d_inode(dentry)) {
		agfs_cache_perm(d_inode(dentry), dentry);

		/* Hidden entries appear as if they don't exist. */
		if (AGFS_SB(dentry->d_sb)->permission &&
		    AGFS_I(d_inode(dentry))->cached_perm == AGFS_PERM_HIDE) {
			d_drop(dentry);
			return ERR_PTR(-ENOENT);
		}
	}
	return NULL;
}
