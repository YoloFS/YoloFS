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
	/* kind = AGFS_DKIND_PASSTHROUGH (0), in_base = false — from zalloc */
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
 * Revert a staged dentry to passthrough.  No-op if already passthrough
 * — calling dput on a passthrough dentry would drop a reference that
 * was never acquired by dget, causing a refcount underflow.
 * Caller must hold i_rwsem exclusive on the parent.
 */
void agfs_dentry_unstage(struct dentry *dentry)
{
	struct agfs_dentry_info *di = AGFS_D(dentry);

	if (di->kind == AGFS_DKIND_PASSTHROUGH)
		return;

	di->kind = AGFS_DKIND_PASSTHROUGH;
	di->in_base = false;
	dput(dentry);
}

/*
 * Set a dentry's overlay state, handling the passthrough → staged
 * transition (dget pin) and overwrite.
 * Caller must hold i_rwsem exclusive on the parent directory.
 */
void agfs_dentry_stage(struct dentry *dentry, enum agfs_dkind kind,
		       bool in_base)
{
	struct agfs_dentry_info *di = AGFS_D(dentry);
	bool was_passthrough = (di->kind == AGFS_DKIND_PASSTHROUGH);

	di->kind = kind;
	di->in_base = in_base;

	if (was_passthrough)
		dget(dentry);
}

/*
 * Stage a dentry as a staged inode with the current generation.
 * Used by create and COW where new content is being staged.
 * Rename uses agfs_dentry_stage directly (gen unchanged).
 */
void agfs_dentry_stage_inode(struct dentry *dentry, struct agfs_sb_info *sbi,
			     bool in_base)
{
	agfs_dentry_stage(dentry, AGFS_DKIND_STAGED_INODE, in_base);
	AGFS_I(d_inode(dentry))->staging_gen = (u16)atomic_read(&sbi->gen);
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

/* ── Tombstone operations ──────────────────────────────────────────── */

/*
 * Create a negative (tombstone) dentry at @name under @parent and
 * stage it.  The d_alloc() reference serves as the pin — no extra
 * dget().
 *
 * Returns the tombstone dentry, or NULL on allocation failure.
 * Caller must hold i_rwsem exclusive on dir.
 */
struct dentry *agfs_dentry_add_tombstone(struct dentry *parent,
					 const char *name, unsigned int len)
{
	struct dentry *tomb;

	tomb = agfs_d_alloc(parent, name, len);
	if (!tomb)
		return NULL;

	AGFS_D(tomb)->kind = AGFS_DKIND_TOMBSTONE;
	AGFS_D(tomb)->in_base = true;
	d_add(tomb, NULL);
	return tomb;
}

/*
 * Undo agfs_dentry_add_tombstone: unhash, clear, and release.
 * Used for rollback when a subsequent step (e.g., journal write) fails.
 * Caller must hold i_rwsem exclusive on dir.
 */
void agfs_dentry_remove_tombstone(struct dentry *tomb)
{
	d_drop(tomb);
	agfs_dentry_unstage(tomb);
}

/* ── Inject helper (restore path) ──────────────────────────────────── */

/*
 * Create a VFS dentry under @parent, attach the resolved @lower_path,
 * iget the inode, and add to the dcache.
 * Takes ownership of @lower_path — released on error via dput → d_release.
 */
int agfs_dentry_inject(struct dentry *parent, const u8 *name,
		       u16 name_len, struct super_block *sb,
		       struct path *lower_path,
		       enum agfs_dkind kind, bool in_base, u16 gen)
{
	struct dentry *child;
	struct inode *inode;

	child = agfs_d_alloc(parent, (const char *)name, name_len);
	if (!child) {
		path_put(lower_path);
		return -ENOMEM;
	}

	agfs_set_lower_path(child, lower_path);
	AGFS_D(child)->kind = kind;
	AGFS_D(child)->in_base = in_base;
	inode = agfs_iget(sb, d_inode(lower_path->dentry));
	if (IS_ERR(inode)) {
		dput(child);
		return PTR_ERR(inode);
	}
	if (kind == AGFS_DKIND_STAGED_INODE)
		AGFS_I(inode)->staging_gen = gen;
	d_add(child, inode);
	return 0;
}

/* ── Bulk unstage ──────────────────────────────────────────────────── */

/*
 * Iteratively unstage all staged child dentries via depth-first walk.
 *
 * The hlist traversal is lockless — holding d_lock across the loop is
 * not possible because agfs_dentry_unstage() calls dput(), which may
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
		    AGFS_D(cur)->kind != AGFS_DKIND_PASSTHROUGH)
			agfs_dentry_unstage(cur);
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
