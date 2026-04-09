// SPDX-License-Identifier: GPL-2.0
/*
 * yolofs — dentry operations.
 */

#include "yolofs.h"

static struct kmem_cache *yolo_dentry_cachep;

/* ── Dentry lifecycle ──────────────────────────────────────────────── */

/*
 * d_init callback — auto-initialize yolo_dentry_info on every dentry
 * at allocation time.  This replaces the manual
 * yolo_new_dentry_private_data() call and ensures negative dentries
 * created via d_alloc() have d_fsdata ready.
 */
static int yolo_d_init(struct dentry *dentry)
{
	struct yolo_dentry_info *info;

	info = kmem_cache_zalloc(yolo_dentry_cachep, GFP_KERNEL);
	if (!info)
		return -ENOMEM;

	spin_lock_init(&info->lock);
	/* Ground state: unpinned, following base filesystem */
	info->target = YOLO_TARGET_PATH;
	info->perm = YOLO_PERM_NONE;
	INIT_LIST_HEAD(&info->rule_pin);
	info->rule_dentry = NULL;
	dentry->d_fsdata = info;
	return 0;
}

static void yolo_d_release(struct dentry *dentry)
{
	struct yolo_dentry_info *info = YOLO_D(dentry);

	if (!info)
		return;

	yolo_put_reset_lower_path(dentry);
	kmem_cache_free(yolo_dentry_cachep, info);
	dentry->d_fsdata = NULL;
}

/* ── Dentry state API ──────────────────────────────────────────────── */

/*
 * Interpose the yolofs layer on @dentry: wrap the lower inode with an yolofs
 * inode, store the lower path, and splice into dcache.
 *
 * Three entry scenarios:
 *   NULL @lower_path     — tombstone: leave/add negative, no lower path.
 *   Negative @lower_path — lookup miss: release ref, leave/add negative.
 *   Positive @lower_path — lookup hit / create: wrap lower inode, store
 *                          lower path, add to dcache.
 *
 * Adapts to both unhashed dentries (lookup — d_add) and VFS-hashed
 * dentries (create — d_instantiate).
 *
 * Caller must pass either a fresh negative dentry or a VFS-hashed negative
 * dentry.  Re-instantiating an already-positive dentry is a bug.
 *
 * Consumes @lower_path in all cases (puts on negative/error, stores on
 * success).  Today the negative/tombstone outcomes only come from
 * yolo_lookup() lookup misses and yolo_dentry_create(..., NULL), both of
 * which start from fresh negative dentries, so those branches do not need to
 * rewrite lower_path.
 */
int yolo_dentry_interpose(struct dentry *dentry, struct path *lower_path)
{
	struct inode *inode = NULL;
	bool unhashed = d_unhashed(dentry);

	if (WARN_ON_ONCE(d_really_is_positive(dentry))) {
		if (lower_path)
			path_put(lower_path);
		return -EEXIST;
	}

	if (!lower_path) {
		/*
		 * Tombstone — overlay state says this name must stay absent.
		 * Delete, rename, and jump create these intentionally, so
		 * there is no lower backing path to keep on the dentry.
		 */
	} else if (d_is_negative(lower_path->dentry)) {
		/*
		 * Negative lower — base lookup missed.  This is not a staged
		 * tombstone, just "nothing in base right now".  Drop the
		 * temporary lower ref; no caller reads lower_path from a
		 * negative lookup dentry.
		 */
		path_put(lower_path);
	} else {
		/*
		 * Positive lower — either a base lookup hit or a newly created
		 * staged inode.  Wrap the lower inode in an yolofs inode, then
		 * transfer the owned lower_path ref onto this dentry so later
		 * opens/attrs resolve through the chosen backing object.
		 */
		inode = yolo_iget(dentry->d_sb, d_inode(lower_path->dentry));
		if (IS_ERR(inode)) {
			path_put(lower_path);
			return PTR_ERR(inode);
		}

		yolo_replace_lower_path(dentry, lower_path);
	}

	/*
	 * Fresh lookup/d_alloc dentries need d_add().  For VFS-hashed dentries
	 * the positive case uses d_instantiate(); negative/tombstone outcomes
	 * are already represented by the existing negative dentry in cache.
	 */
	if (unhashed)
		d_add(dentry, inode);
	else if (inode)
		d_instantiate(dentry, inode);
	return 0;
}

/*
 * Allocate a child dentry under @parent, pin it with @target, and
 * interpose into the dcache.  The child comes from d_alloc(), so the
 * interpose step always starts from a fresh negative dentry.
 * On success the caller loses ownership of @lower_path (if non-NULL).
 * Caller must hold i_rwsem exclusive on the parent directory.
 */
struct dentry *yolo_dentry_create(struct dentry *parent,
				  const char *name, unsigned int len,
				  enum yolo_target target,
				  struct path *lower_path)
{
	struct qstr qname;
	struct dentry *child;
	int err;

	qname.name = (const unsigned char *)name;
	qname.len = len;
	qname.hash = full_name_hash(parent, name, len);
	child = d_alloc(parent, &qname);
	if (!child) {
		if (lower_path)
			path_put(lower_path);
		return ERR_PTR(-ENOMEM);
	}

	YOLO_D(child)->pinned = true;	/* d_alloc ref counts as pin */
	YOLO_D(child)->target = target;

	err = yolo_dentry_interpose(child, lower_path);
	if (err) {
		dput(child);
		return ERR_PTR(err);
	}

	return child;
}

/*
 * Set a dentry's overlay target and pin it.  The only unpinned state
 * is ground state, reached via yolo_dentry_unpin().
 * Caller must hold i_rwsem exclusive on the parent directory.
 */
void yolo_dentry_pin(struct dentry *dentry, enum yolo_target target)
{
	struct yolo_dentry_info *di = YOLO_D(dentry);
	bool was_pinned = di->pinned;

	di->target = target;
	di->pinned = true;

	if (!was_pinned)
		dget(dentry);
}

/*
 * Return a dentry to ground state — staging no longer has interest
 * in it.  The target field becomes don't-care; lookups fall
 * through to base as if staging never touched this entry.
 * Caller must hold i_rwsem exclusive on the parent directory.
 */
void yolo_dentry_unpin(struct dentry *dentry)
{
	struct yolo_dentry_info *di = YOLO_D(dentry);
	bool was_pinned = di->pinned;

	di->target = YOLO_TARGET_PATH;
	di->pinned = false;

	if (was_pinned) {
		if (d_is_negative(dentry))
			d_drop(dentry);
		dput(dentry);
	}
}

/* ── Bulk unpin ─────────────────────────────────────────────────────── */

/*
 * Iteratively unpin all pinned child dentries via depth-first walk.
 *
 * The hlist traversal is lockless — holding d_lock across the loop is
 * not possible because yolo_dentry_unpin() calls dput(), which may
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
void yolo_dentry_unpin_all(struct super_block *sb)
{
	struct hlist_node *pos[YOLO_JUMP_MAX_DEPTH];
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

		/* Descend into children before unpinning this entry */
		if (!hlist_empty(&cur->d_children) &&
		    depth + 1 < YOLO_JUMP_MAX_DEPTH)
			pos[++depth] = cur->d_children.first;

		if (YOLO_D(cur) && YOLO_D(cur)->pinned)
			yolo_dentry_unpin(cur);
	}

	shrink_dcache_sb(sb);
}

const struct dentry_operations yolo_dops = {
	.d_init		= yolo_d_init,
	.d_release	= yolo_d_release,
};

int yolo_init_dentry_cache(void)
{
	yolo_dentry_cachep = kmem_cache_create("yolo_dentry_cache",
					       sizeof(struct yolo_dentry_info),
					       0, SLAB_RECLAIM_ACCOUNT, NULL);
	return yolo_dentry_cachep ? 0 : -ENOMEM;
}

void yolo_destroy_dentry_cache(void)
{
	if (yolo_dentry_cachep)
		kmem_cache_destroy(yolo_dentry_cachep);
}
