// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — staging layer helpers.
 *
 * Flat inode store, directory pinning, COW.
 */

#include "agfs.h"
#include <linux/fs.h>
#include <linux/file.h>
#include <linux/namei.h>

/* ── Relative Path from Dentry ─────────────────────────────────────── */

int agfs_dentry_relpath(struct dentry *dentry, char *buf, int buflen)
{
	char *p;

	p = dentry_path_raw(dentry, buf, buflen);
	if (IS_ERR(p))
		return PTR_ERR(p);

	/* dentry_path_raw returns pointer into buf; shift if needed */
	if (p != buf)
		memmove(buf, p, strlen(p) + 1);
	return 0;
}

/* ── Public Helpers ────────────────────────────────────────────────── */

int agfs_inode_path(struct agfs_sb_info *sbi, u32 ino,
		    struct path *result)
{
	char name[11];

	if (!sbi->inodes_dir.dentry)
		return -ENOENT;

	snprintf(name, sizeof(name), "%u", ino);
	/* No LOOKUP_FOLLOW — symlink inodes must not be dereferenced */
	return vfs_path_lookup(sbi->inodes_dir.dentry, sbi->inodes_dir.mnt,
			       name, 0, result);
}

/* ── Inode Store Allocation ────────────────────────────────────────── */

/*
 * Allocate a new inode ID, create the inode in the store.
 * Regular files get vfs_create; dirs get vfs_mkdir; symlinks get vfs_symlink.
 */
int agfs_inode_alloc(struct agfs_sb_info *sbi, u32 *out_ino,
		     struct path *inode_path, umode_t mode,
		     const char *symname)
{
	char name[11];
	struct dentry *ino_dentry;
	struct inode *dir;
	u32 ino;
	int err, len;

	if (!sbi->inodes_dir.dentry)
		return -ENOENT;

	ino = (u32)atomic_inc_return(&sbi->next_ino);
	if (unlikely(ino == 0))
		return -ENOSPC;
	len = snprintf(name, sizeof(name), "%u", ino);

	dir = d_inode(sbi->inodes_dir.dentry);
	inode_lock(dir);
	ino_dentry = lookup_one_len(name, sbi->inodes_dir.dentry, len);
	if (IS_ERR(ino_dentry)) {
		inode_unlock(dir);
		return PTR_ERR(ino_dentry);
	}

	if (S_ISDIR(mode))
		err = vfs_mkdir(mnt_idmap(sbi->inodes_dir.mnt),
				dir, ino_dentry, mode);
	else if (S_ISLNK(mode))
		err = vfs_symlink(mnt_idmap(sbi->inodes_dir.mnt),
				  dir, ino_dentry, symname);
	else
		err = vfs_create(mnt_idmap(sbi->inodes_dir.mnt),
				 dir, ino_dentry, mode, true);

	inode_unlock(dir);
	if (err) {
		dput(ino_dentry);
		return err;
	}

	inode_path->dentry = ino_dentry;
	inode_path->mnt = mntget(sbi->inodes_dir.mnt);
	*out_ino = ino;
	return 0;
}

/* ── Copy File Content to Inode Store ──────────────────────────────── */

static int agfs_copy_to_inode(struct dentry *dentry,
			      const struct path *inode_path)
{
	struct file *src, *dst;
	struct path lower_path;
	loff_t len, copied_total = 0;
	ssize_t copied;
	int err = 0;

	agfs_get_lower_path(dentry, &lower_path);
	src = dentry_open(&lower_path, O_RDONLY, current_cred());
	agfs_put_lower_path(dentry, &lower_path);
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

int agfs_do_cow(struct agfs_sb_info *sbi, struct dentry *dentry,
		struct file **new_file, int flags, bool truncate)
{
	struct agfs_dentry_info *di = AGFS_D(dentry);
	struct inode *parent = d_inode(dentry->d_parent);
	struct path inode_path;
	u32 ino;
	int err;

	err = agfs_inode_alloc(sbi, &ino, &inode_path,
			       d_inode(dentry)->i_mode & ~S_IFMT, NULL);
	if (err)
		return err;

	/* Copy base content (skip when truncating — inode stays empty) */
	if (!truncate) {
		err = agfs_copy_to_inode(dentry, &inode_path);
		if (err) {
			path_put(&inode_path);
			return err;
		}
	}

	/*
	 * Set packed on dentry and pin if needed.
	 * Take inode_lock(parent) to serialize against VFS-driven
	 * create/unlink/rename on the same directory.
	 */
	inode_lock(parent);
	if (list_empty(&di->de_node)) {
		agfs_stage_dentry(dentry, parent,
				  agfs_dstate_inode(ino,
						(u16)atomic_read(&sbi->gen),
						DT_REG, true));
	} else {
		agfs_dstate_free(di->packed);
		di->packed = agfs_dstate_inode(ino,
					   (u16)atomic_read(&sbi->gen),
					   DT_REG, true);
	}
	inode_unlock(parent);

	path_get(&inode_path); /* extra ref for reopen below */

	/* Update dentry lower_path to point at the inode (consumes original ref) */
	agfs_replace_lower_path(dentry, &inode_path);

	/* Append journal record (best-effort — packed is already set) */
	agfs_journal_modify(sbi, dentry, ino, DT_REG);

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

/* ── Release Pinned Directory Inodes ───────────────────────────────── */

void agfs_release_pinned_dirs(struct agfs_sb_info *sbi)
{
	LIST_HEAD(local);
	struct agfs_inode_info *ii, *tmp;

	spin_lock(&sbi->pinned_dirs_lock);
	list_splice_init(&sbi->pinned_dirs, &local);
	spin_unlock(&sbi->pinned_dirs_lock);

	list_for_each_entry_safe(ii, tmp, &local, de_pin) {
		struct agfs_dentry_info *di, *di_tmp;

		list_del_init(&ii->de_pin);
		inode_lock(&ii->vfs_inode);
		list_for_each_entry_safe(di, di_tmp, &ii->de_list, de_node)
			agfs_unstage_dentry(di);
		inode_unlock(&ii->vfs_inode);
		/* No iput() — child dput() cascades to parent via d_parent refs */
	}
}
