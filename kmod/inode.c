// SPDX-License-Identifier: GPL-2.0
/*
 * yolofs — inode operations.
 *
 * Directory operations (create, mkdir, unlink, rmdir, rename, symlink),
 * permission checking, setattr, getattr.
 */

#include "yolofs.h"
#include <linux/xattr.h>

/* ── Permission check for metadata (directory) operations ─────────── */

/*
 * Check if a metadata operation (create, mkdir, unlink, rename, symlink)
 * is allowed.  Uses the parent directory's permission (metadata ops are
 * mutations of the parent).  Reuses the same resolve→ask→check pipeline
 * as yolo_open.
 */
static int yolo_check_mutate_perm(struct dentry *dentry)
{
	struct yolo_sb_info *sbi = YOLO_SB(dentry->d_sb);
	bool ask_resolved = false;
	int err;

	if (!sbi->perm.enabled)
		return 0;

	/* Mutates are gated on the parent's perm; record the child as the
	 * target but resolve the blocking rule from the parent (checked). */
	err = yolo_perm_check_dentry(sbi, dentry->d_parent, O_WRONLY,
				     &ask_resolved);
	if (err == -EACCES && !ask_resolved)
		yolo_journal_block(sbi, dentry, dentry->d_parent, YOLO_OP_WRITE);
	return err;
}

/* ── create/mkdir/symlink — allocate inode + set up dentry ────────── */

static int yolo_create_staged(struct inode *dir, struct dentry *dentry,
			      umode_t mode, const char *symname)
{
	struct yolo_sb_info *sbi = YOLO_SB(dir->i_sb);
	struct path inode_path;
	u32 ino;
	int err;

	err = yolo_inode_alloc(sbi, &ino, &inode_path, mode, symname);
	if (err)
		return err;

	/* Journal before publishing the dentry: a failed append (e.g. ENOSPC)
	 * must fail the create with nothing visible in the mount, matching
	 * delete/rename. dentry_path_raw works on the still-negative dentry.
	 * Fresh create/mkdir/symlink — nothing existed before, so pre = "a". */
	err = yolo_journal_stage(sbi, dentry, ino, "a");
	if (err) {
		path_put(&inode_path);
		return err;
	}

	/* Publish. interpose consumes inode_path (stores on success, puts on
	 * error). A post-journal interpose failure is the rare ENOMEM path: it
	 * leaves a harmless phantom S record for an empty orphan inode (cleaned
	 * on commit/abort) — the safe direction, never an unjournaled change. */
	err = yolo_dentry_interpose(dentry, &inode_path);
	if (err)
		return err;

	yolo_dentry_pin(dentry, YOLO_TARGET_INODE);
	YOLO_I(d_inode(dentry))->staging_gen = (u16)atomic_read(&sbi->staging.gen);
	YOLO_I(d_inode(dentry))->staging_ino = ino;

	return 0;
}

static int yolo_create(struct mnt_idmap *idmap, struct inode *dir,
		       struct dentry *dentry, umode_t mode, bool excl)
{
	int err = yolo_check_mutate_perm(dentry);
	if (err)
		return err;
	return yolo_create_staged(dir, dentry, mode, NULL);
}

static int yolo_mkdir(struct mnt_idmap *idmap, struct inode *dir,
		      struct dentry *dentry, umode_t mode)
{
	int err = yolo_check_mutate_perm(dentry);
	if (err)
		return err;
	return yolo_create_staged(dir, dentry, S_IFDIR | mode, NULL);
}

/* ── unlink/rmdir — negative entry or remove entry ───────────────── */

static int yolo_delete_entry(struct inode *dir, struct dentry *dentry)
{
	struct yolo_sb_info *sbi = YOLO_SB(dentry->d_sb);
	struct dentry *tomb;
	int err;

	err = yolo_check_mutate_perm(dentry);
	if (err)
		return err;

	/* Pre-allocate negative dentry (tombstone) */
	tomb = yolo_dentry_create(dentry->d_parent,
				  dentry->d_name.name,
				  dentry->d_name.len,
				  YOLO_TARGET_NONE, NULL);
	if (IS_ERR(tomb))
		return PTR_ERR(tomb);

	/* Journal (uses dentry path, must be before d_drop) */
	err = yolo_journal_delete(sbi, dentry);
	if (err) {
		yolo_dentry_unpin(tomb);
		return err;
	}

	/* Release pinned state (if any) on the original dentry before eviction */
	yolo_dentry_unpin(dentry);
	d_drop(dentry);
	return 0;
}

/* ── symlink ───────────────────────────────────────────────────────── */

static int yolo_symlink(struct mnt_idmap *idmap, struct inode *dir,
			struct dentry *dentry, const char *symname)
{
	int err = yolo_check_mutate_perm(dentry);
	if (err)
		return err;
	return yolo_create_staged(dir, dentry, S_IFLNK, symname);
}

/* ── rename ────────────────────────────────────────────────────────── */

static int yolo_rename(struct mnt_idmap *idmap,
		       struct inode *old_dir, struct dentry *old_dentry,
		       struct inode *new_dir, struct dentry *new_dentry,
		       unsigned int flags)
{
	struct yolo_sb_info *sbi = YOLO_SB(old_dentry->d_sb);
	struct dentry *tomb;
	int err;

	/* Check write permission on both source and destination dirs. */
	err = yolo_check_mutate_perm(old_dentry);
	if (err)
		return err;
	err = yolo_check_mutate_perm(new_dentry);
	if (err)
		return err;

	if (flags)
		return -EINVAL;

	/*
	 * Always tombstone at old name, even if the source had no base
	 * content.  A spurious tombstone (hiding nothing in base) is
	 * harmless — lookup returns ENOENT, readdir skips it, and commit
	 * silently ignores a D for a non-existent base path.
	 */
	tomb = yolo_dentry_create(old_dentry->d_parent,
				  old_dentry->d_name.name,
				  old_dentry->d_name.len,
				  YOLO_TARGET_NONE, NULL);
	if (IS_ERR(tomb))
		return PTR_ERR(tomb);

	/* Journal BEFORE d_move (uses dentry paths) */
	err = yolo_journal_rename(sbi, old_dentry, new_dentry);
	if (err)
		goto out_tomb;

	/*
	 * Unhash both dentries before modifying pin state.
	 * VFS will call d_move(old_dentry, new_dentry) after we return,
	 * rehashing old_dentry at the new position.
	 */
	d_drop(old_dentry);
	d_drop(new_dentry);

	/* Release staging state on new_dentry (being replaced) */
	yolo_dentry_unpin(new_dentry);

	/* Pin old_dentry at its new position so it survives dcache pressure */
	yolo_dentry_pin(old_dentry, YOLO_D(old_dentry)->target);

	return 0;

out_tomb:
	yolo_dentry_unpin(tomb);
	return err;
}

/* ── permission ────────────────────────────────────────────────────── */

static int yolo_permission(struct mnt_idmap *idmap,
			   struct inode *inode, int mask)
{
	struct yolo_inode_info *info = YOLO_I(inode);
	struct yolo_sb_info *sbi = YOLO_SB(inode->i_sb);
	enum yolo_perm perm;
	struct inode *lower_inode = yolo_lower_inode(inode);
	struct dentry *alias;

	/* Skip all permission gating if disabled */
	if (!sbi->perm.enabled)
		return 0;

	/* Check generation — re-resolve if stale */
	if (info->perm_gen != atomic64_read(&sbi->perm.gen)) {
		struct dentry *dentry = d_find_alias(inode);
		if (dentry) {
			yolo_perm_refresh(inode, dentry);
			dput(dentry);
		}
	}
	perm = info->cached_perm;

	/* Hidden paths return ENOENT regardless of type */
	if (perm == YOLO_PERM_HIDE)
		return -ENOENT;

	/* Directories: delegate to lower FS (deny still allows traversal) */
	if (!S_ISREG(inode->i_mode))
		return inode_permission(idmap, lower_inode, mask);

	/* ALLOW passes. ASK and WRITE_ASK pass too — asks are resolved in
	 * open()/metadata-op paths where sleeping is safe. Only READ_ONLY
	 * writes, DENY, and unexpected values fall through to -EACCES. */
	switch (perm) {
	case YOLO_PERM_ALLOW:
	case YOLO_PERM_ASK:
	case YOLO_PERM_WRITE_ASK:
		return 0;
	case YOLO_PERM_READ_ONLY:
		if (!(mask & MAY_WRITE))
			return 0;
		break;
	default:
		break;
	}

	/* -EACCES path: a static deny/read-only block (ASK/WRITE_ASK returned 0
	 * above and never reach here, so this is never an ask-resolved deny).
	 * Log a B against the inode's dentry; it is both target and checked. */
	alias = d_find_alias(inode);
	if (alias) {
		enum yolo_op op = (mask & MAY_WRITE) ? YOLO_OP_WRITE : YOLO_OP_READ;
		yolo_journal_block(sbi, alias, alias, op);
		dput(alias);
	}
	return -EACCES;
}

/* ── setattr ───────────────────────────────────────────────────────── */

static bool yolo_setattr_needs_write_check(const struct iattr *ia)
{
	unsigned int mutating = ATTR_MODE | ATTR_UID | ATTR_GID | ATTR_SIZE |
				ATTR_ATIME | ATTR_MTIME |
				ATTR_ATIME_SET | ATTR_MTIME_SET;

	return (ia->ia_valid & mutating) && !(ia->ia_valid & ATTR_OPEN);
}

static int yolo_setattr(struct mnt_idmap *idmap,
			struct dentry *dentry, struct iattr *ia)
{
	struct yolo_sb_info *sbi = YOLO_SB(dentry->d_sb);
	struct path lower_path;
	struct inode *inode = d_inode(dentry);
	struct inode *lower_inode;
	int err;

	if (sbi->perm.enabled && yolo_setattr_needs_write_check(ia)) {
		bool ask_resolved = false;

		err = yolo_perm_check_dentry(sbi, dentry, O_WRONLY,
					     &ask_resolved);
		if (err) {
			/* Static block only — an ask-resolved deny is already an A. */
			if (err == -EACCES && !ask_resolved)
				yolo_journal_block(sbi, dentry, dentry,
						   YOLO_OP_WRITE);
			return err;
		}
	}

	err = setattr_prepare(idmap, dentry, ia);
	if (err)
		return err;

	/* If truncating, sync upper inode size */
	if (ia->ia_valid & ATTR_SIZE) {
		err = inode_newsize_ok(inode, ia->ia_size);
		if (err)
			return err;
		truncate_setsize(inode, ia->ia_size);

		/* When staging is active, don't propagate size changes
		 * to the base file — the staging copy is the data store.
		 * The VFS triggers this via O_TRUNC after open; the
		 * staging file is already the correct size. */
		if (sbi->staging.enabled && S_ISREG(inode->i_mode))
			ia->ia_valid &= ~ATTR_SIZE;
	}

	/* If nothing left to propagate, we're done */
	if (!ia->ia_valid)
		return 0;

	yolo_get_lower_path(dentry, &lower_path);
	lower_inode = d_inode(lower_path.dentry);

	inode_lock(lower_inode);
	err = notify_change(mnt_idmap(lower_path.mnt), lower_path.dentry,
			    ia, NULL);
	inode_unlock(lower_inode);

	if (!err) {
		fsstack_copy_attr_all(inode, lower_inode);
		fsstack_copy_inode_size(inode, lower_inode);
	}

	yolo_put_lower_path(dentry, &lower_path);
	return err;
}

/* ── getattr ───────────────────────────────────────────────────────── */

static int yolo_getattr(struct mnt_idmap *idmap,
			const struct path *path, struct kstat *stat,
			u32 request_mask, unsigned int query_flags)
{
	struct dentry *dentry = path->dentry;
	struct inode *inode = d_inode(dentry);
	struct yolo_sb_info *sbi = YOLO_SB(inode->i_sb);
	struct path lower_path;
	struct inode *lower_inode;
	int err;

	/* Hidden paths return ENOENT on stat. */
	if (sbi->perm.enabled &&
	    yolo_perm_get(inode, dentry) == YOLO_PERM_HIDE)
		return -ENOENT;

	yolo_get_lower_path(dentry, &lower_path);
	lower_inode = d_inode(lower_path.dentry);
	err = vfs_getattr_nosec(&lower_path, stat, request_mask, query_flags);
	if (!err)
		fsstack_copy_attr_all(inode, lower_inode);
	yolo_put_lower_path(dentry, &lower_path);

	if (!err)
		stat->dev = dentry->d_sb->s_dev;
	return err;
}

/* ── listxattr ─────────────────────────────────────────────────────── */

static ssize_t yolo_listxattr(struct dentry *dentry, char *buffer,
			      size_t buffer_size)
{
	struct path lower_path;
	ssize_t err;

	yolo_get_lower_path(dentry, &lower_path);
	err = vfs_listxattr(lower_path.dentry, buffer, buffer_size);
	yolo_put_lower_path(dentry, &lower_path);
	return err;
}

/* ── symlink iops ──────────────────────────────────────────────────── */

static const char *yolo_get_link(struct dentry *dentry, struct inode *inode,
				 struct delayed_call *done)
{
	const char *link;
	struct dentry *lower_dentry;

	if (!dentry)
		return ERR_PTR(-ECHILD);

	lower_dentry = yolo_lower_dentry(dentry);
	if (!lower_dentry || !d_inode(lower_dentry) ||
	    !d_inode(lower_dentry)->i_op->get_link)
		return ERR_PTR(-ENOENT);

	link = d_inode(lower_dentry)->i_op->get_link(lower_dentry,
						     d_inode(lower_dentry),
						     done);
	return link;
}

/* ── Ops Tables ────────────────────────────────────────────────────── */

const struct inode_operations yolo_dir_iops = {
	.lookup		= yolo_lookup,
	.create		= yolo_create,
	.mkdir		= yolo_mkdir,
	.unlink		= yolo_delete_entry,
	.rmdir		= yolo_delete_entry,
	.symlink	= yolo_symlink,
	.rename		= yolo_rename,
	.permission	= yolo_permission,
	.setattr	= yolo_setattr,
	.getattr	= yolo_getattr,
	.listxattr	= yolo_listxattr,
};

const struct inode_operations yolo_main_iops = {
	.permission	= yolo_permission,
	.setattr	= yolo_setattr,
	.getattr	= yolo_getattr,
	.listxattr	= yolo_listxattr,
};

const struct inode_operations yolo_symlink_iops = {
	.get_link	= yolo_get_link,
	.permission	= yolo_permission,
	.setattr	= yolo_setattr,
	.getattr	= yolo_getattr,
	.listxattr	= yolo_listxattr,
};
