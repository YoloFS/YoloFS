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
	info->ovr_buckets = NULL;
	dentry->d_fsdata = info;
	return 0;
}

void agfs_free_dentry_private_data(struct dentry *dentry)
{
	struct agfs_dentry_info *info = AGFS_D(dentry);
	struct agfs_override *ovr;
	struct hlist_node *tmp;
	unsigned int i;

	if (info->ovr_buckets) {
		for (i = 0; i < AGFS_OVR_BUCKETS; i++) {
			hlist_for_each_entry_safe(ovr, tmp,
						  &info->ovr_buckets[i], node) {
				hlist_del(&ovr->node);
				kfree(ovr->base_path);
				kfree(ovr);
			}
		}
		kfree(info->ovr_buckets);
	}
	kmem_cache_free(agfs_dentry_cachep, info);
	dentry->d_fsdata = NULL;
}

static int agfs_d_revalidate(struct dentry *dentry, unsigned int flags)
{
	struct path lower_path;
	struct dentry *lower_dentry;
	int err = 1;

	if (flags & LOOKUP_RCU)
		return -ECHILD;

	if (!AGFS_D(dentry))
		return 0;

	agfs_get_lower_path(dentry, &lower_path);
	lower_dentry = lower_path.dentry;

	if (lower_dentry && lower_dentry->d_op && lower_dentry->d_op->d_revalidate)
		err = lower_dentry->d_op->d_revalidate(lower_dentry, flags);

	agfs_put_lower_path(dentry, &lower_path);
	return err;
}

static void agfs_d_release(struct dentry *dentry)
{
	if (!AGFS_D(dentry))
		return;

	agfs_put_reset_lower_path(dentry);
	agfs_free_dentry_private_data(dentry);
}

const struct dentry_operations agfs_dops = {
	.d_revalidate	= agfs_d_revalidate,
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
