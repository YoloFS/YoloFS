// SPDX-License-Identifier: GPL-2.0
/*
 * yolofs — staging layer helpers.
 *
 * Sharded inode store, directory pinning, COW.
 */

#include "yolofs.h"
#include <linux/fs.h>
#include <linux/file.h>
#include <linux/namei.h>

/* ── Lower-fs helpers (used only here; version-compat lives in them) ─── */

/*
 * Look up @name in lower directory @base; caller holds i_rwsem on @base.
 * lookup_one_len() became qstr-based lookup_one() in 7.0.
 */
static struct dentry *yolo_lower_lookup_locked(struct mnt_idmap *idmap, const char *name,
					       struct dentry *base, unsigned int len)
{
#if LINUX_VERSION_CODE >= KERNEL_VERSION(7, 0, 0)
	struct qstr q = QSTR_LEN(name, len);

	return lookup_one(idmap, &q, base);
#else
	return lookup_one_len(name, base, len);
#endif
}

/*
 * Create a node of any type under @dentry in the lower fs, dispatching by mode:
 * directory -> vfs_mkdir, symlink -> vfs_symlink, else vfs_create. Caller holds
 * i_rwsem on @dir. The three helpers' signatures all changed across 6.8..7.0
 * (vfs_mkdir's dentry return in 6.15, delegated-inode args in 7.0), so the
 * version handling is confined here.
 *
 * Returns the dentry now backing the entry. The caller owns a reference to it;
 * for mkdir on >= 6.15 it may be a *different* dentry than @dentry, in which
 * case the caller must dput() the one it passed in. Returns ERR_PTR on failure.
 */
static struct dentry *yolo_lower_create(struct mnt_idmap *idmap, struct inode *dir,
					struct dentry *dentry, umode_t mode,
					const char *symname)
{
	int err;

	if (S_ISDIR(mode)) {
		struct dentry *made;

#if LINUX_VERSION_CODE >= KERNEL_VERSION(7, 0, 0)
		made = vfs_mkdir(idmap, dir, dentry, mode, NULL);
#elif LINUX_VERSION_CODE >= KERNEL_VERSION(6, 15, 0)
		made = vfs_mkdir(idmap, dir, dentry, mode);
#else
		err = vfs_mkdir(idmap, dir, dentry, mode);
		made = err ? ERR_PTR(err) : dentry;
#endif
		if (IS_ERR(made))
			return made;
		return made ? made : dentry;	/* NULL ⇒ passed-in dentry was used */
	}

	if (S_ISLNK(mode))
#if LINUX_VERSION_CODE >= KERNEL_VERSION(7, 0, 0)
		err = vfs_symlink(idmap, dir, dentry, symname, NULL);
#else
		err = vfs_symlink(idmap, dir, dentry, symname);
#endif
	else
#if LINUX_VERSION_CODE >= KERNEL_VERSION(7, 0, 0)
		err = vfs_create(idmap, dentry, mode, NULL);
#else
		err = vfs_create(idmap, dir, dentry, mode, true);
#endif

	return err ? ERR_PTR(err) : dentry;
}

#define YOLO_SHARD_SIZE	100

/* ── Shard Helpers ─────────────────────────────────────────────────── */

/*
 * Get the shard directory for a given ino, creating it if needed.
 * Returns the shard dentry with an extra dget() ref, or ERR_PTR.
 * Caller must dput() when done.
 *
 * The last shard dentry is cached in sbi to avoid repeated lookups
 * (sequential inos hit the same shard ~1000 times in a row).
 */
static struct dentry *yolo_get_shard_dir(struct yolo_sb_info *sbi, u32 ino)
{
	u32 shard = ino / YOLO_SHARD_SIZE;
	char shard_name[11];
	struct dentry *shard_dentry, *old;
	struct inode *dir;
	u64 epoch;
	int len;

	/* Fast path: cached shard matches. */
	spin_lock(&sbi->staging.shard_lock);
	if (sbi->staging.shard_dentry && sbi->staging.shard_id == shard) {
		shard_dentry = dget(sbi->staging.shard_dentry);
		spin_unlock(&sbi->staging.shard_lock);
		return shard_dentry;
	}
	epoch = sbi->staging.shard_epoch;
	spin_unlock(&sbi->staging.shard_lock);

	len = snprintf(shard_name, sizeof(shard_name), "%u", shard);
	dir = d_inode(sbi->staging.inodes_dir.dentry);

	inode_lock(dir);
	shard_dentry = yolo_lower_lookup_locked(mnt_idmap(sbi->staging.inodes_dir.mnt),
				       shard_name,
				       sbi->staging.inodes_dir.dentry, len);
	if (IS_ERR(shard_dentry)) {
		inode_unlock(dir);
		return shard_dentry;
	}

	if (!d_inode(shard_dentry)) {
		/* Shard dir doesn't exist yet — create it. */
		struct dentry *made = yolo_lower_create(
				mnt_idmap(sbi->staging.inodes_dir.mnt),
				dir, shard_dentry, S_IFDIR | 0755, NULL);
		if (IS_ERR(made)) {
			inode_unlock(dir);
			dput(shard_dentry);
			return made;
		}
		/* vfs_mkdir() may hand back a different dentry (>= 6.15). */
		if (made != shard_dentry) {
			dput(shard_dentry);
			shard_dentry = made;
		}
	}
	inode_unlock(dir);

	/* Update cache; drop the displaced entry's ref outside the lock. Skip
	 * the publish if quiesce invalidated the cache while we looked up —
	 * this dentry belongs to the old view and must not outlive it there. */
	spin_lock(&sbi->staging.shard_lock);
	if (sbi->staging.shard_epoch == epoch) {
		old = sbi->staging.shard_dentry;
		sbi->staging.shard_dentry = dget(shard_dentry);
		sbi->staging.shard_id = shard;
	} else {
		old = NULL;
	}
	spin_unlock(&sbi->staging.shard_lock);
	if (old)
		dput(old);

	return shard_dentry;
}

/* ── Public Helpers ────────────────────────────────────────────────── */

int yolo_inode_path(struct yolo_sb_info *sbi, u32 ino,
		    struct path *result)
{
	char rel[24]; /* "<shard>/<ino>" */

	if (!sbi->staging.inodes_dir.dentry)
		return -ENOENT;

	snprintf(rel, sizeof(rel), "%u/%u", ino / YOLO_SHARD_SIZE, ino);
	/* No LOOKUP_FOLLOW — symlink inodes must not be dereferenced */
	return vfs_path_lookup(sbi->staging.inodes_dir.dentry, sbi->staging.inodes_dir.mnt,
			       rel, 0, result);
}

/* ── Inode Store Allocation ────────────────────────────────────────── */

/*
 * Allocate a new inode ID and create its backing object (file/dir/symlink,
 * per @mode) in the sharded store via yolo_lower_create().
 */
int yolo_inode_alloc(struct yolo_sb_info *sbi, u32 *out_ino,
		     struct path *inode_path, umode_t mode,
		     const char *symname)
{
	char name[11];
	struct dentry *shard_dentry;
	struct dentry *ino_dentry;
	struct dentry *created;
	struct inode *shard_inode;
	u32 ino;
	int err, len;

	if (!sbi->staging.inodes_dir.dentry)
		return -ENOENT;

	ino = (u32)atomic_inc_return(&sbi->staging.next_ino);
	if (unlikely(ino == 0))
		return -ENOSPC;

	shard_dentry = yolo_get_shard_dir(sbi, ino);
	if (IS_ERR(shard_dentry))
		return PTR_ERR(shard_dentry);

	len = snprintf(name, sizeof(name), "%u", ino);
	shard_inode = d_inode(shard_dentry);

	inode_lock(shard_inode);
	ino_dentry = yolo_lower_lookup_locked(mnt_idmap(sbi->staging.inodes_dir.mnt),
				     name, shard_dentry, len);
	if (IS_ERR(ino_dentry)) {
		inode_unlock(shard_inode);
		dput(shard_dentry);
		return PTR_ERR(ino_dentry);
	}

	created = yolo_lower_create(mnt_idmap(sbi->staging.inodes_dir.mnt),
				    shard_inode, ino_dentry, mode, symname);
	if (IS_ERR(created)) {
		err = PTR_ERR(created);
	} else {
		/* mkdir may hand back a different dentry (>= 6.15). */
		if (created != ino_dentry) {
			dput(ino_dentry);
			ino_dentry = created;
		}
		err = 0;
	}

	inode_unlock(shard_inode);
	dput(shard_dentry);

	if (err) {
		dput(ino_dentry);
		return err;
	}

	inode_path->dentry = ino_dentry;
	inode_path->mnt = mntget(sbi->staging.inodes_dir.mnt);
	*out_ino = ino;
	return 0;
}

/* ── Copy File Content to Inode Store ──────────────────────────────── */

static int yolo_copy_to_inode(struct dentry *dentry,
			      const struct path *inode_path)
{
	struct file *src, *dst;
	struct path lower_path;
	loff_t len, copied_total = 0;
	ssize_t copied;
	int err = 0;

	yolo_get_lower_path(dentry, &lower_path);
	src = dentry_open(&lower_path, O_RDONLY, current_cred());
	yolo_put_lower_path(dentry, &lower_path);
	if (IS_ERR(src))
		return PTR_ERR(src);

	dst = dentry_open(inode_path, O_WRONLY | O_TRUNC, current_cred());
	if (IS_ERR(dst)) {
		fput(src);
		return PTR_ERR(dst);
	}

	len = i_size_read(file_inode(src));
	while (copied_total < len) {
		size_t chunk = min_t(loff_t, len - copied_total, 1 << 20);

		copied = vfs_copy_file_range(src, copied_total,
					     dst, copied_total, chunk, 0);
		if (copied <= 0) {
			if (copied < 0)
				err = copied;
			break;
		}
		copied_total += copied;
	}

	fput(dst);
	fput(src);
	return err;
}

/* ── Copy-on-Write to Inode Store ──────────────────────────────────── */

int yolo_do_cow(struct yolo_sb_info *sbi, struct dentry *dentry,
		struct file **new_file, int flags, bool truncate)
{
	struct inode *parent = d_inode(dentry->d_parent);
	struct path inode_path;
	char pre_buf[YOLO_PATH_MAX];
	const char *pre;
	u32 ino;
	int err;

	/* Capture the pre-op backing — the lower we're about to copy up — before
	 * the lower_path is swapped to the new inode below. Tagged as a/s:/b: so
	 * `yolo review --diff` can read the previous-snapshot content in O(segment).
	 * For a re-COW of an already-staged file the dentry still carries the OLD
	 * staging_ino here, so this resolves to the prior snapshot's s:<ino>. */
	pre = yolo_preimage_target(dentry, pre_buf, sizeof(pre_buf));

	err = yolo_inode_alloc(sbi, &ino, &inode_path,
			       d_inode(dentry)->i_mode & ~S_IFMT, NULL);
	if (err)
		return err;

	/* Copy base content (skip when truncating — inode stays empty) */
	if (!truncate) {
		err = yolo_copy_to_inode(dentry, &inode_path);
		if (err) {
			path_put(&inode_path);
			return err;
		}
	}

	/* Journal before publishing the new backing — a failed append (e.g.
	 * ENOSPC, or ENAMETOOLONG past YOLO_PATH_MAX) must fail the open with the
	 * previous mapping still authoritative, matching delete/rename. `pre`
	 * (captured above, before any state change) is the previous-snapshot
	 * content the CLI diffs against; "a" if it couldn't be resolved. The
	 * orphaned store inode is cleaned up on the next commit/abort. */
	err = yolo_journal_stage(sbi, dentry, ino, pre);
	if (err) {
		path_put(&inode_path);
		return err;
	}

	/*
	 * Publish: set target on dentry and pin if needed.
	 * Take inode_lock(parent) to serialize against VFS-driven
	 * create/unlink/rename on the same directory.
	 */
	inode_lock(parent);
	yolo_dentry_pin(dentry, YOLO_TARGET_INODE);
	yolo_stamp_staged(dentry, (u16)atomic_read(&sbi->staging.gen), ino);
	inode_unlock(parent);

	path_get(&inode_path); /* extra ref for reopen below */

	/* Update dentry lower_path to point at the inode (consumes original ref) */
	yolo_replace_lower_path(dentry, &inode_path);

	/* Reopen with requested flags */
	err = 0;
	if (new_file) {
		*new_file = dentry_open(&inode_path, flags, current_cred());
		if (IS_ERR(*new_file)) {
			err = PTR_ERR(*new_file);
			*new_file = NULL;
		}
	}
	path_put(&inode_path); /* drop extra ref */
	return err;
}
