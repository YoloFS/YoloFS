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

	if (!sbi->perm.enabled)
		return 0;

	/* Mutates are gated on the parent's perm (check); a block reports the
	 * child (target). */
	return yolo_perm_check_dentry(sbi, dentry->d_parent, dentry,
				      YOLO_OP_WRITE);
}

/* ── create/mkdir/symlink — allocate inode + set up dentry ────────── */

static int yolo_stage_inode(struct inode *dir, struct dentry *dentry,
			      umode_t mode, const char *symname)
{
	struct yolo_sb_info *sbi = YOLO_SB(dir->i_sb);
	struct path inode_path;
	u32 ino;
	int err;

	err = yolo_check_mutate_perm(dentry);
	if (err)
		return err;

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

	yolo_dentry_pin(dentry, YOLO_BACKING_STAGED);
	yolo_stamp_staged(dentry, (u16)atomic_read(&sbi->staging.gen), ino);

	return 0;
}

static int yolo_create(struct mnt_idmap *idmap, struct inode *dir,
		       struct dentry *dentry, umode_t mode, bool excl)
{
	return yolo_stage_inode(dir, dentry, mode, NULL);
}

/*
 * ->mkdir's return type changed from int to struct dentry * (NULL on success)
 * in 6.15. Both forms are a thin wrapper over the shared int-returning
 * yolo_stage_inode(); only the signature and the success/error mapping differ.
 */
#if LINUX_VERSION_CODE >= KERNEL_VERSION(6, 15, 0)
static struct dentry *yolo_mkdir(struct mnt_idmap *idmap, struct inode *dir,
				 struct dentry *dentry, umode_t mode)
{
	int err = yolo_stage_inode(dir, dentry, S_IFDIR | mode, NULL);

	return err ? ERR_PTR(err) : NULL;
}
#else
static int yolo_mkdir(struct mnt_idmap *idmap, struct inode *dir,
		      struct dentry *dentry, umode_t mode)
{
	return yolo_stage_inode(dir, dentry, S_IFDIR | mode, NULL);
}
#endif

static int yolo_symlink(struct mnt_idmap *idmap, struct inode *dir,
			struct dentry *dentry, const char *symname)
{
	return yolo_stage_inode(dir, dentry, S_IFLNK, symname);
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
				  YOLO_BACKING_NONE, NULL);
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
				  YOLO_BACKING_NONE, NULL);
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
	yolo_dentry_pin(old_dentry, YOLO_D(old_dentry)->backing);

	return 0;

out_tomb:
	yolo_dentry_unpin(tomb);
	return err;
}

/* ── permission ────────────────────────────────────────────────────── */

/*
 * Regular-file ACCESS (allow/deny/read-only/ask) is NOT decided here — it is
 * enforced authoritatively in yolo_open(), which holds the exact dentry and
 * may sleep. This callback only has the inode, so it must not resolve
 * name-based access.
 *
 * Regular files pass here unconditionally: a write is COW'd into staging, so
 * the lower file's unix mode is irrelevant (a read-only base is writable
 * through the mount); a read still opens the lower file, whose mode the lower
 * FS enforces at open.
 *
 * Consequence: access(2)/faccessat(2) on a regular file does not reflect the
 * yolo access policy — it reports success; open() is the real gate.
 * Directories/symlinks delegate to the lower FS (traversal, dir mode bits);
 * a `deny` directory's *listing* is blocked in yolo_readdir, not here.
 */
static int yolo_permission(struct mnt_idmap *idmap,
			   struct inode *inode, int mask)
{
	/* Gating disabled: fully transparent — don't impose the lower FS's mode
	 * bits (staging can COW into a read-only base). */
	if (!YOLO_SB(inode->i_sb)->perm.enabled)
		return 0;

	if (S_ISREG(inode->i_mode))
		return 0;

	return inode_permission(idmap, yolo_lower_inode(inode), mask);
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
		/* check == target: the file's own perm gates the setattr. */
		err = yolo_perm_check_dentry(sbi, dentry, dentry, YOLO_OP_WRITE);
		if (err)
			return err;
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
	struct path lower_path;
	struct inode *lower_inode;
	int err;

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
