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
	info->dstate = (struct agfs_dstate){0}; /* passthrough */
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
 * AgFS only supports local lower filesystems (ext4, xfs, …) that never
 * set DCACHE_OP_REVALIDATE.  Mount is rejected for remote filesystems
 * (e.g. NFS) that need revalidation.  Since agfs_dops omits
 * d_revalidate, the VFS keeps lookup_fast in pure RCU-walk mode.
 */

static void agfs_d_release(struct dentry *dentry)
{
	struct agfs_dentry_info *info = AGFS_D(dentry);

	if (!info)
		return;

	agfs_dstate_free(info->dstate);
	agfs_put_reset_lower_path(dentry);
	agfs_free_dentry_private_data(dentry);
}

/* ── Dentry Staging Helpers ────────────────────────────────────────── */

/*
 * Stage a VFS-provided dentry.  Sets dstate and takes a dget() pin
 * so the dentry persists in d_children.
 * Caller must hold i_rwsem exclusive on the parent directory.
 */
void agfs_stage_dentry(struct dentry *dentry, struct agfs_dstate dstate)
{
	AGFS_D(dentry)->dstate = dstate;
	dget(dentry);	/* pin in dcache so it stays in d_children */
}

/*
 * Remove a dentry from staging: free dstate, clear to passthrough,
 * release pin.  The agfs_dentry_info (and its dentry) may be freed
 * after this call if the dput drops the last reference.
 * Caller must hold i_rwsem exclusive on the parent directory.
 */
void agfs_unstage_dentry(struct agfs_dentry_info *di)
{
	agfs_dstate_free(di->dstate);
	di->dstate = (struct agfs_dstate){0}; /* passthrough */
	dput(di->dentry);
}

/*
 * Create a negative (tombstone) dentry at @name under @parent and
 * stage it.  The d_alloc() reference serves as the pin — no extra
 * dget().
 *
 * Returns the tombstone dentry, or NULL on allocation failure.
 * Caller must hold i_rwsem exclusive on dir.
 */
struct dentry *agfs_add_tombstone(struct dentry *parent,
				  const char *name, unsigned int len,
				  unsigned char d_type)
{
	struct qstr qname = QSTR_INIT(name, len);
	struct dentry *tomb;

	qname.hash = full_name_hash(parent, name, len);
	tomb = d_alloc(parent, &qname);
	if (!tomb)
		return NULL;

	AGFS_D(tomb)->dstate = agfs_dstate_tombstone(d_type);
	d_add(tomb, NULL);
	return tomb;
}

/*
 * Undo agfs_add_tombstone: unhash, clear dstate, and release.
 * Used for rollback when a subsequent step (e.g., journal write) fails.
 * Caller must hold i_rwsem exclusive on dir.
 */
void agfs_remove_tombstone(struct dentry *tomb)
{
	d_drop(tomb);
	agfs_unstage_dentry(AGFS_D(tomb));
}

/*
 * Iteratively unstage all staged child dentries via depth-first walk.
 *
 * The hlist traversal is lockless — holding d_lock across the loop is
 * not possible because agfs_unstage_dentry() calls dput(), which may
 * re-acquire d_lock and deadlock.  To make the lockless walk safe we
 * call shrink_dcache_sb() first: this evicts every unreferenced
 * (passthrough) dentry, so every entry still in d_children has a
 * positive refcount and cannot be freed mid-iteration.  Concurrent
 * lookups only hlist_add_head (at the front) which does not disturb
 * our forward ->next traversal.
 *
 * A second shrink_dcache_sb() after the walk evicts the dentries that
 * were just unstaged (dput drops their refcount but leaves them cached
 * on the LRU), so subsequent VFS lookups go through the module again.
 */
void agfs_unstage_all(struct super_block *sb)
{
	struct hlist_node *pos[AGFS_RESTORE_MAX_DEPTH];
	struct dentry *cur;
	int depth = 0;

	if (!sb->s_root)
		return;

	shrink_dcache_sb(sb);

	if (hlist_empty(&sb->s_root->d_children))
		return;

	pos[0] = sb->s_root->d_children.first;

	while (depth >= 0) {
		if (!pos[depth]) {
			depth--;
			continue;
		}

		cur = hlist_entry(pos[depth], struct dentry, d_sib);
		pos[depth] = pos[depth]->next;

		/* Descend into children before unstaging this entry */
		if (!hlist_empty(&cur->d_children) &&
		    depth + 1 < AGFS_RESTORE_MAX_DEPTH)
			pos[++depth] = cur->d_children.first;

		if (AGFS_D(cur) &&
		    !agfs_dstate_is_passthrough(AGFS_D(cur)->dstate))
			agfs_unstage_dentry(AGFS_D(cur));
	}

	shrink_dcache_sb(sb);
}

const struct dentry_operations agfs_dops = {
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
