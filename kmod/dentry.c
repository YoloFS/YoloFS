// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — dentry operations.
 */

#include "agfs.h"

static struct kmem_cache *agfs_dentry_cachep;

int agfs_new_dentry_private_data(struct dentry *dentry)
{
	struct agfs_dentry_info *info;

	info = kmem_cache_zalloc(agfs_dentry_cachep, GFP_ATOMIC);
	if (!info)
		return -ENOMEM;

	spin_lock_init(&info->lock);
	info->perm = AGFS_PERM_NONE;
	INIT_LIST_HEAD(&info->rule_pin);
	info->rule_dentry = NULL;
	dentry->d_fsdata = info;
	return 0;
}

void agfs_free_dentry_private_data(struct dentry *dentry)
{
	struct agfs_dentry_info *info = AGFS_D(dentry);
	kmem_cache_free(agfs_dentry_cachep, info);
	dentry->d_fsdata = NULL;
}

/*
 * Dentry revalidation.
 *
 * For local lower filesystems (ext4, xfs, …) the lower dentry never has
 * d_revalidate, so there is nothing to proxy.  In that common case we can
 * return 1 immediately — including under RCU-walk — so that lookup_fast
 * stays on the fast RCU path and avoids the refcount bouncing that
 * path_get/path_put would cause.
 *
 * If the lower filesystem *does* set DCACHE_OP_REVALIDATE (e.g. NFS),
 * we fall back to ref-walk and proxy the call, exactly like overlayfs
 * does in ovl_revalidate_real().
 */
static int agfs_d_revalidate(struct dentry *dentry, unsigned int flags)
{
	struct agfs_dentry_info *info;
	struct dentry *lower_dentry;

	if (flags & LOOKUP_RCU) {
		if (!d_inode_rcu(dentry))
			return -ECHILD;
		info = AGFS_D(dentry);
		if (!info)
			return -ECHILD;
		lower_dentry = info->lower_path.dentry;
		/* No lower revalidate → dentry is valid, stay in RCU-walk. */
		if (!lower_dentry ||
		    !(lower_dentry->d_flags & DCACHE_OP_REVALIDATE))
			return 1;
		/* Lower needs revalidation — drop to ref-walk. */
		return -ECHILD;
	}

	if (!AGFS_D(dentry))
		return 0;

	/* ref-walk: proxy to the lower dentry's revalidate if it has one. */
	{
		struct path lower_path;
		int err = 1;

		agfs_get_lower_path(dentry, &lower_path);
		lower_dentry = lower_path.dentry;

		if (lower_dentry &&
		    (lower_dentry->d_flags & DCACHE_OP_REVALIDATE))
			err = lower_dentry->d_op->d_revalidate(lower_dentry,
							       flags);

		agfs_put_lower_path(dentry, &lower_path);
		return err;
	}
}

static void agfs_d_release(struct dentry *dentry)
{
	if (!AGFS_D(dentry))
		return;

	agfs_put_reset_lower_path(dentry);
	agfs_free_dentry_private_data(dentry);
}

/* Full ops: proxy d_revalidate to the lower filesystem (e.g. NFS). */
const struct dentry_operations agfs_dops = {
	.d_revalidate	= agfs_d_revalidate,
	.d_release	= agfs_d_release,
};

/* Fast ops: no d_revalidate — for local lower filesystems (ext4, xfs).
 * The VFS won't set DCACHE_OP_REVALIDATE on these dentries, so
 * lookup_fast stays in pure RCU-walk without any function call. */
const struct dentry_operations agfs_dops_fast = {
	.d_release	= agfs_d_release,
};

int agfs_init_dentry_cache(void)
{
	agfs_dentry_cachep = kmem_cache_create("agfs_dentry_cache",
					       sizeof(struct agfs_dentry_info),
					       0, SLAB_RECLAIM_ACCOUNT, NULL);
	return agfs_dentry_cachep ? 0 : -ENOMEM;
}

void agfs_destroy_dentry_cache(void)
{
	if (agfs_dentry_cachep)
		kmem_cache_destroy(agfs_dentry_cachep);
}
