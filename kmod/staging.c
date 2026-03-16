// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — staging layer helpers.
 *
 * Flat inode store, dirent hash table management, COW.
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

/* ── Resolve a sub-path under a given root ─────────────────────────── */

static int resolve_subpath(const struct path *root, const char *relpath,
			   struct path *result)
{
	return vfs_path_lookup(root->dentry, root->mnt, relpath,
			       LOOKUP_FOLLOW, result);
}

/* ── Public Helpers ────────────────────────────────────────────────── */

int agfs_base_path(struct agfs_sb_info *sbi, const char *relpath,
		   struct path *result)
{
	return resolve_subpath(&sbi->base_path, relpath, result);
}

int agfs_inode_path(struct agfs_sb_info *sbi, u64 ino,
		    struct path *result)
{
	char name[21];

	if (!sbi->inodes_dir.dentry)
		return -ENOENT;

	snprintf(name, sizeof(name), "%llu", (unsigned long long)ino);
	return resolve_subpath(&sbi->inodes_dir, name, result);
}

/* ── Stage Dirent Hash Table ───────────────────────────────────────────── */

static inline unsigned int agfs_de_hash(const char *name, unsigned int len)
{
	return full_name_hash(NULL, name, len) >> (32 - AGFS_DE_SHIFT);
}

/*
 * Find a dirent by name. Caller must hold dii->de_lock.
 */
struct agfs_dirent *agfs_find_dirent(struct inode *dir,
					 const char *name,
					 unsigned int namelen)
{
	struct agfs_inode_info *dii = AGFS_I(dir);
	struct agfs_dirent *de;
	unsigned int idx;

	if (!dii->de_buckets)
		return NULL;

	idx = agfs_de_hash(name, namelen);
	hlist_for_each_entry(de, &dii->de_buckets[idx], node) {
		if (de->name_len == namelen &&
		    !memcmp(de->name, name, namelen))
			return de;
	}
	return NULL;
}

/*
 * Free all dirent entries and the bucket array on a directory inode.
 */
static void agfs_free_de_buckets(struct agfs_inode_info *dii)
{
	struct hlist_head *buckets;
	struct agfs_dirent *de;
	struct hlist_node *tmp;
	unsigned int i;

	spin_lock(&dii->de_lock);
	buckets = dii->de_buckets;
	dii->de_buckets = NULL;
	spin_unlock(&dii->de_lock);

	if (!buckets)
		return;

	for (i = 0; i < AGFS_DE_BUCKETS; i++) {
		hlist_for_each_entry_safe(de, tmp, &buckets[i], node) {
			hlist_del(&de->node);
			kfree(de->base_path);
			kfree(de);
		}
	}
	kfree(buckets);
}

/*
 * Lazily allocate the bucket array for a directory inode.
 * Sets *first = true if this call created the array (caller must pin).
 */
static int agfs_ensure_de_buckets(struct agfs_inode_info *dii, bool *first)
{
	struct hlist_head *buckets;
	unsigned int i;

	*first = false;
	if (dii->de_buckets)
		return 0;

	buckets = kmalloc_array(AGFS_DE_BUCKETS, sizeof(struct hlist_head),
				GFP_KERNEL);
	if (!buckets)
		return -ENOMEM;
	for (i = 0; i < AGFS_DE_BUCKETS; i++)
		INIT_HLIST_HEAD(&buckets[i]);

	spin_lock(&dii->de_lock);
	if (!dii->de_buckets) {
		dii->de_buckets = buckets;
		buckets = NULL;
		*first = true;
	}
	spin_unlock(&dii->de_lock);

	kfree(buckets);
	return 0;
}

/*
 * Pin a directory inode on first dirent insertion so it survives eviction.
 */
static int agfs_pin_dir(struct agfs_inode_info *dii, struct agfs_sb_info *sbi)
{
	if (!igrab(&dii->vfs_inode)) {
		agfs_free_de_buckets(dii);
		return -EIO;
	}
	spin_lock(&sbi->pinned_dirs_lock);
	list_add(&dii->de_pin, &sbi->pinned_dirs);
	spin_unlock(&sbi->pinned_dirs_lock);
	return 0;
}

/*
 * Add or update a dirent. All-zero de means deleted.
 * On first dirent for a directory, pins the inode via igrab().
 *
 * Callers hold inode_lock(dir) or staging_sem, so no concurrent writer
 * can race between the find-miss and the insert — no retry needed.
 */
int agfs_del_dirent(struct inode *dir, const char *name,
		      unsigned int namelen)
{
	return agfs_add_dirent(dir, name, namelen, &(struct agfs_dirent){0});
}

int agfs_add_dirent(struct inode *dir, const char *name,
		      unsigned int namelen,
		      const struct agfs_dirent *de)
{
	struct agfs_inode_info *dii = AGFS_I(dir);
	struct agfs_dirent *old_de, *new_de;
	char *bp_copy = NULL;
	bool first_de;
	int err;

	if (de->base_path) {
		bp_copy = kstrdup(de->base_path, GFP_KERNEL);
		if (!bp_copy)
			return -ENOMEM;
	}

	err = agfs_ensure_de_buckets(dii, &first_de);
	if (err) {
		kfree(bp_copy);
		return err;
	}

	/* Fast path: update existing entry in place */
	spin_lock(&dii->de_lock);
	old_de = agfs_find_dirent(dir, name, namelen);
	if (old_de) {
		kfree(old_de->base_path);
		old_de->ino = de->ino;
		old_de->base_path = bp_copy;
		old_de->d_type = de->d_type;
		old_de->snapshot_gen = de->snapshot_gen;
		spin_unlock(&dii->de_lock);
		goto out;
	}
	spin_unlock(&dii->de_lock);

	/* Slow path: allocate new entry and insert */
	new_de = kmalloc(offsetof(struct agfs_dirent, name) + namelen + 1,
			 GFP_KERNEL);
	if (!new_de) {
		kfree(bp_copy);
		return -ENOMEM;
	}
	memcpy(new_de->name, name, namelen);
	new_de->name[namelen] = '\0';
	new_de->name_len = namelen;
	new_de->ino = de->ino;
	new_de->base_path = bp_copy;
	new_de->d_type = de->d_type;
	new_de->snapshot_gen = de->snapshot_gen;

	spin_lock(&dii->de_lock);
	hlist_add_head(&new_de->node,
		       &dii->de_buckets[agfs_de_hash(name, namelen)]);
	spin_unlock(&dii->de_lock);

out:
	if (first_de)
		return agfs_pin_dir(dii, AGFS_SB(dir->i_sb));
	return 0;
}

/* ── Inode Store Allocation ────────────────────────────────────────── */

/*
 * Allocate a new inode ID, create the inode in the store.
 * Regular files get vfs_create; dirs get vfs_mkdir; symlinks get vfs_symlink.
 */
int agfs_inode_alloc(struct agfs_sb_info *sbi, u64 *out_ino,
		     struct path *inode_path, umode_t mode,
		     const char *symname)
{
	char name[21];
	struct dentry *ino_dentry;
	struct inode *dir;
	u64 ino;
	int err;

	if (!sbi->inodes_dir.dentry)
		return -ENOENT;

	ino = atomic64_inc_return(&sbi->next_ino);
	snprintf(name, sizeof(name), "%llu", (unsigned long long)ino);

	dir = d_inode(sbi->inodes_dir.dentry);
	inode_lock(dir);
	ino_dentry = lookup_one_len(name, sbi->inodes_dir.dentry,
				    strlen(name));
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
	struct path inode_path;
	u64 ino;
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
	 * Add dirent on parent directory inode.
	 *
	 * NOTE: if this fails the inode file remains on disk unreferenced.
	 * Orphaned inodes are cleaned up on the next `agfs commit` or
	 * `agfs abort` because the entire inode store is removed.
	 */
	err = agfs_add_dirent(d_inode(dentry->d_parent),
				dentry->d_name.name,
				dentry->d_name.len,
				&(struct agfs_dirent){
					.ino = ino,
					.d_type = DT_REG,
					.snapshot_gen = (u64)atomic64_read(
							&sbi->snapshot_gen),
				});
	if (err) {
		path_put(&inode_path);
		return err;
	}

	path_get(&inode_path); /* extra ref for reopen below */

	/* Update dentry lower_path to point at the inode (consumes original ref) */
	agfs_replace_lower_path(dentry, &inode_path);

	/* Append journal record (best-effort — dirent is already set) */
	{
		char buf[AGFS_PATH_MAX];

		if (!agfs_dentry_relpath(dentry, buf, sizeof(buf)))
			agfs_journal_append_a(sbi, buf, ino);
	}

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
		list_del_init(&ii->de_pin);
		agfs_free_de_buckets(ii);
		iput(&ii->vfs_inode);
	}
}
