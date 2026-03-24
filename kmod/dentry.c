// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — dentry operations.
 */

#include "agfs.h"

static struct kmem_cache *agfs_dentry_cachep;

/* ── Dentry lifecycle ──────────────────────────────────────────────── */

/*
 * d_init callback — auto-initialize agfs_dentry_info on every dentry
 * at allocation time.  This replaces the manual
 * agfs_new_dentry_private_data() call and ensures negative dentries
 * created via d_alloc() have d_fsdata ready.
 */
static int agfs_d_init(struct dentry *dentry)
{
	struct agfs_dentry_info *info;

	info = kmem_cache_zalloc(agfs_dentry_cachep, GFP_KERNEL);
	if (!info)
		return -ENOMEM;

	spin_lock_init(&info->lock);
	/* pinned = false, in_base = false — from zalloc; target is don't-care when !pinned */
	info->perm = AGFS_PERM_NONE;
	INIT_LIST_HEAD(&info->rule_pin);
	info->rule_dentry = NULL;
	dentry->d_fsdata = info;
	return 0;
}

static void agfs_d_release(struct dentry *dentry)
{
	struct agfs_dentry_info *info = AGFS_D(dentry);

	if (!info)
		return;

	agfs_put_reset_lower_path(dentry);
	kmem_cache_free(agfs_dentry_cachep, info);
	dentry->d_fsdata = NULL;
}

/* ── Dentry-centric mutations ──────────────────────────────────────── */

/*
 * Set a dentry's overlay state.  Handles pin/unpin transitions
 * internally: the only unpinned state is (NONE, false); all others
 * are pinned so the VFS cannot evict them.
 * Caller must hold i_rwsem exclusive on the parent directory.
 */
void agfs_dentry_set(struct dentry *dentry, enum agfs_target target,
		     bool in_base)
{
	struct agfs_dentry_info *di = AGFS_D(dentry);

	/*
	 * Pin any dentry that represents a staged change — the VFS must
	 * not evict it or lookups would fall through to base incorrectly.
	 *
	 *   (INODE, *)    — staged content, must stay visible
	 *   (PATH,  *)    — redirect, must intercept lookups
	 *   (NONE,  true) — tombstone hiding a base entry
	 *   (NONE,  false) — ground state, nothing to preserve → unpin
	 */
	bool should_pin = target != AGFS_TARGET_NONE || in_base;
	bool was_pinned = di->pinned;

	di->target = target;
	di->in_base = in_base;
	di->pinned = should_pin;

	if (should_pin && !was_pinned)
		dget(dentry);
	if (!should_pin && was_pinned) {
		if (d_is_negative(dentry))
			d_drop(dentry);
		dput(dentry);
	}
}

/*
 * Return a dentry to ground state — staging no longer has interest
 * in it.  The target/in_base fields become don't-care; lookups fall
 * through to base as if staging never touched this entry.
 * Caller must hold i_rwsem exclusive on the parent directory.
 */
void agfs_dentry_reset(struct dentry *dentry)
{
	agfs_dentry_set(dentry, AGFS_TARGET_NONE, false);
}

/* ── Child dentry allocation ───────────────────────────────────────── */

static struct dentry *agfs_d_alloc(struct dentry *parent,
				   const char *name, u16 name_len)
{
	struct qstr qname;

	qname.name = (const unsigned char *)name;
	qname.len = name_len;
	qname.hash = full_name_hash(parent, name, name_len);
	return d_alloc(parent, &qname);
}

/* ── Dentry allocation ──────────────────────────────────────────────── */

/*
 * Allocate a child dentry under @parent and pre-pin it.
 * The d_alloc() reference serves as the pin — no extra dget().
 * Caller must call d_add() after configuring the dentry.
 *
 * Returns the pre-pinned dentry, or NULL on allocation failure.
 * Caller must hold i_rwsem exclusive on dir.
 */
struct dentry *agfs_dentry_alloc(struct dentry *parent,
			       const char *name, unsigned int len)
{
	struct dentry *d;

	d = agfs_d_alloc(parent, name, len);
	if (!d)
		return NULL;

	AGFS_D(d)->pinned = true;	/* d_alloc ref counts as pin */
	return d;
}

/* ── Bulk reset ─────────────────────────────────────────────────────── */

/*
 * Iteratively reset all pinned child dentries via depth-first walk.
 *
 * The hlist traversal is lockless — holding d_lock across the loop is
 * not possible because agfs_dentry_reset() calls dput(), which may
 * re-acquire d_lock and deadlock.  To make the lockless walk safe we call
 * shrink_dcache_sb() first: this evicts every unreferenced (unpinned)
 * dentry, so every entry still in d_children has a positive refcount
 * and cannot be freed mid-iteration.  Concurrent lookups only
 * hlist_add_head (at the front) which does not disturb our forward
 * ->next traversal.
 *
 * A second shrink_dcache_sb() after the walk evicts the dentries that
 * were just unpinned (dput drops their refcount but leaves them cached
 * on the LRU), so subsequent VFS lookups go through the module again.
 */
void agfs_dentry_reset_all(struct super_block *sb)
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

		/* Descend into children before resetting this entry */
		if (!hlist_empty(&cur->d_children) &&
		    depth + 1 < AGFS_RESTORE_MAX_DEPTH)
			pos[++depth] = cur->d_children.first;

		if (AGFS_D(cur) && AGFS_D(cur)->pinned)
			agfs_dentry_reset(cur);
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
