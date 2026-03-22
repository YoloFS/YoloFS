// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — dentry operations.
 */

#include "agfs.h"

static struct kmem_cache *agfs_dentry_cachep;

static void agfs_free_dentry_private_data(struct dentry *dentry)
{
	struct agfs_dentry_info *info = AGFS_D(dentry);
	kmem_cache_free(agfs_dentry_cachep, info);
	dentry->d_fsdata = NULL;
}

/*
 * d_init callback — auto-initialize agfs_dentry_info on every dentry
 * at allocation time.  This replaces the manual
 * agfs_new_dentry_private_data() call and ensures tombstone dentries
 * created via d_alloc() have d_fsdata ready.
 */
static int agfs_d_init(struct dentry *dentry)
{
	struct agfs_dentry_info *info;

	info = kmem_cache_zalloc(agfs_dentry_cachep, GFP_KERNEL);
	if (!info)
		return -ENOMEM;

	spin_lock_init(&info->lock);
	info->dstate = (struct agfs_dstate){0}; /* untracked */
	INIT_LIST_HEAD(&info->de_node);
	info->dentry = dentry;
	info->perm = AGFS_PERM_NONE;
	INIT_LIST_HEAD(&info->rule_pin);
	info->rule_dentry = NULL;
	dentry->d_fsdata = info;
	return 0;
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
	struct agfs_dentry_info *info = AGFS_D(dentry);

	if (!info)
		return;

	WARN_ON_ONCE(!list_empty(&info->de_node));
	agfs_dstate_free(info->dstate);
	agfs_put_reset_lower_path(dentry);
	agfs_free_dentry_private_data(dentry);
}

/* ── Dentry Staging Helpers ────────────────────────────────────────── */

/*
 * Add dir to sbi->pinned_dirs if not already there.
 * No igrab() needed — pinned child dentries hold a ref on d_parent,
 * which transitively keeps the parent inode alive.
 */
void agfs_pin_dir_if_first(struct agfs_inode_info *dii,
			   struct agfs_sb_info *sbi)
{
	if (!list_empty(&dii->de_pin))
		return;
	spin_lock(&sbi->pinned_dirs_lock);
	if (list_empty(&dii->de_pin))
		list_add(&dii->de_pin, &sbi->pinned_dirs);
	spin_unlock(&sbi->pinned_dirs_lock);
}

/*
 * Stage a VFS-provided dentry on its parent directory's de_list.
 * Takes a dget() pin.  Caller must hold i_rwsem exclusive on dir.
 */
void agfs_stage_dentry(struct dentry *dentry, struct inode *dir,
		       struct agfs_dstate dstate)
{
	struct agfs_dentry_info *di = AGFS_D(dentry);
	struct agfs_inode_info *dii = AGFS_I(dir);

	di->dstate = dstate;
	dget(dentry);
	list_add(&di->de_node, &dii->de_list);
	agfs_pin_dir_if_first(dii, AGFS_SB(dir->i_sb));
}

/*
 * Remove a dentry from its parent's de_list, free dstate, release pin.
 * The agfs_dentry_info (and its dentry) may be freed after this call
 * if the dput drops the last reference.
 * Caller must hold i_rwsem exclusive on the parent directory.
 */
void agfs_unstage_dentry(struct agfs_dentry_info *di)
{
	list_del_init(&di->de_node);
	agfs_dstate_free(di->dstate);
	di->dstate = (struct agfs_dstate){0}; /* untracked */
	dput(di->dentry);
}

/*
 * Create a negative (tombstone) dentry at @name under @parent and
 * stage it on @dir's de_list.  The d_alloc() reference serves as the
 * pin — no extra dget().
 *
 * Returns the tombstone dentry, or NULL on allocation failure.
 * Caller must hold i_rwsem exclusive on dir.
 */
struct dentry *agfs_add_tombstone(struct dentry *parent,
				  const char *name, unsigned int len,
				  struct inode *dir, unsigned char d_type)
{
	struct agfs_inode_info *dii = AGFS_I(dir);
	struct qstr qname = QSTR_INIT(name, len);
	struct dentry *tomb;

	qname.hash = full_name_hash(parent, name, len);
	tomb = d_alloc(parent, &qname);
	if (!tomb)
		return NULL;

	AGFS_D(tomb)->dstate = agfs_dstate_tombstone(d_type);
	d_add(tomb, NULL);
	list_add(&AGFS_D(tomb)->de_node, &dii->de_list);
	agfs_pin_dir_if_first(dii, AGFS_SB(dir->i_sb));
	return tomb;
}

/*
 * Undo agfs_add_tombstone: remove from de_list, unhash, and release.
 * Used for rollback when a subsequent step (e.g., journal write) fails.
 * Caller must hold i_rwsem exclusive on dir.
 */
void agfs_remove_tombstone(struct dentry *tomb, struct inode *dir)
{
	list_del_init(&AGFS_D(tomb)->de_node);
	d_drop(tomb);
	dput(tomb);
}

/* Full ops: proxy d_revalidate to the lower filesystem (e.g. NFS). */
const struct dentry_operations agfs_dops = {
	.d_init		= agfs_d_init,
	.d_revalidate	= agfs_d_revalidate,
	.d_release	= agfs_d_release,
};

/* Fast ops: no d_revalidate — for local lower filesystems (ext4, xfs).
 * The VFS won't set DCACHE_OP_REVALIDATE on these dentries, so
 * lookup_fast stays in pure RCU-walk without any function call. */
const struct dentry_operations agfs_dops_fast = {
	.d_init		= agfs_d_init,
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
