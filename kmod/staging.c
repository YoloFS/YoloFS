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

/* ── Stage Dirent Hash Table ───────────────────────────────────────────── */

static inline unsigned int agfs_de_hash(const char *name, unsigned int len)
{
	return full_name_hash(NULL, name, len) >> (32 - AGFS_DE_SHIFT);
}

/*
 * Find a dirent by name. Caller must hold dir->i_rwsem (shared or exclusive).
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
 * Free all dirent entries and the bucket array.
 * Caller must hold inode_lock(dir) (exclusive).
 */
static void agfs_free_de_buckets_locked(struct agfs_inode_info *dii)
{
	struct hlist_head *buckets;
	struct agfs_dirent *de;
	struct hlist_node *tmp;
	unsigned int i;

	buckets = dii->de_buckets;
	dii->de_buckets = NULL;

	if (!buckets)
		return;

	for (i = 0; i < AGFS_DE_BUCKETS; i++) {
		hlist_for_each_entry_safe(de, tmp, &buckets[i], node) {
			hlist_del(&de->node);
			agfs_pde_free(de->packed);
			kfree(de);
		}
	}
	kfree(buckets);
}

/*
 * Free all dirent entries and the bucket array on a directory inode.
 * Takes inode_lock internally.
 */
static void agfs_free_de_buckets(struct agfs_inode_info *dii)
{
	inode_lock(&dii->vfs_inode);
	agfs_free_de_buckets_locked(dii);
	inode_unlock(&dii->vfs_inode);
}

/*
 * Lazily allocate the bucket array for a directory inode.
 * Sets *first = true if this call created the array (caller must pin).
 * Caller must hold inode_lock(dir) (exclusive).
 */
static int agfs_ensure_de_buckets(struct agfs_inode_info *dii, bool *first)
{
	unsigned int i;

	*first = false;
	if (dii->de_buckets)
		return 0;

	dii->de_buckets = kmalloc_array(AGFS_DE_BUCKETS,
					sizeof(struct hlist_head),
					GFP_KERNEL);
	if (!dii->de_buckets)
		return -ENOMEM;

	for (i = 0; i < AGFS_DE_BUCKETS; i++)
		INIT_HLIST_HEAD(&dii->de_buckets[i]);

	*first = true;
	return 0;
}

/*
 * Pin a directory inode on first dirent insertion so it survives eviction.
 */
static int agfs_pin_dir(struct agfs_inode_info *dii, struct agfs_sb_info *sbi)
{
	if (!igrab(&dii->vfs_inode)) {
		agfs_free_de_buckets_locked(dii);
		return -EIO;
	}
	spin_lock(&sbi->pinned_dirs_lock);
	list_add(&dii->de_pin, &sbi->pinned_dirs);
	spin_unlock(&sbi->pinned_dirs_lock);
	return 0;
}

/*
 * Add or update a dirent. All-zero de (packed==0) means tombstone.
 * Cancelled-entry removal: when transitioning to tombstone and the
 * existing entry has in_base=false, the entry is removed entirely.
 * Caller must hold inode_lock(dir) (exclusive).
 */
struct agfs_dirent *agfs_del_dirent(struct inode *dir, const char *name,
				   unsigned int namelen)
{
	return agfs_add_dirent(dir, name, namelen, (agfs_pde_t){0});
}

struct agfs_dirent *agfs_add_dirent(struct inode *dir, const char *name,
				    unsigned int namelen, agfs_pde_t packed)
{
	struct agfs_inode_info *dii = AGFS_I(dir);
	struct agfs_dirent *old_de, *new_de;
	char *base_copy = NULL;
	bool first_de;
	int err;

	/* If packed is a link, duplicate the base string */
	if (agfs_pde_is_link(packed)) {
		unsigned char dt = agfs_pde_d_type(packed);

		base_copy = kstrdup(agfs_pde_base(packed), GFP_KERNEL);
		if (!base_copy)
			return ERR_PTR(-ENOMEM);
		packed = agfs_pde_link(base_copy, dt,
				       agfs_pde_in_base(packed));
	}

	err = agfs_ensure_de_buckets(dii, &first_de);
	if (err) {
		kfree(base_copy);
		return ERR_PTR(err);
	}

	/* Update existing entry in place */
	old_de = agfs_find_dirent(dir, name, namelen);
	if (old_de) {
		if (agfs_pde_is_tombstone(packed)) {
			/* Transitioning to tombstone */
			bool was_in_base = agfs_pde_in_base(old_de->packed);

			agfs_pde_free(old_de->packed);
			if (was_in_base) {
				/* Entry was in base: keep as tombstone */
				old_de->packed = (agfs_pde_t){0};
				new_de = old_de;
			} else {
				/* Cancelled entry: in_base=false tombstone
				 * is useless — remove entirely. */
				hlist_del(&old_de->node);
				kfree(old_de);
				new_de = NULL;
			}
		} else {
			agfs_pde_free(old_de->packed);
			old_de->packed = packed;
			new_de = old_de;
		}
		goto out;
	}

	/* Allocate new entry and insert */
	new_de = kmalloc(offsetof(struct agfs_dirent, name) + namelen + 1,
			 GFP_KERNEL);
	if (!new_de) {
		kfree(base_copy);
		return ERR_PTR(-ENOMEM);
	}
	memcpy(new_de->name, name, namelen);
	new_de->name[namelen] = '\0';
	new_de->name_len = namelen;
	/* No prior dirent: if tombstone, path had content (base-only file). */
	if (agfs_pde_is_tombstone(packed))
		new_de->packed = (agfs_pde_t){0}; /* in_base=true is implicit for tombstone */
	else
		new_de->packed = packed;

	hlist_add_head(&new_de->node,
		       &dii->de_buckets[agfs_de_hash(name, namelen)]);

out:
	if (first_de) {
		err = agfs_pin_dir(dii, AGFS_SB(dir->i_sb));
		if (err) {
			/* Undo the insertion we just made. */
			if (new_de) {
				hlist_del(&new_de->node);
				agfs_pde_free(new_de->packed);
				kfree(new_de);
			}
			return ERR_PTR(err);
		}
	}
	return new_de;
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
	int err;

	if (!sbi->inodes_dir.dentry)
		return -ENOENT;

	ino = atomic_inc_return(&sbi->next_ino);
	snprintf(name, sizeof(name), "%u", ino);

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
	struct agfs_dirent *de;
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
	 * Add dirent on parent directory inode.
	 * Take inode_lock(parent) to serialize against VFS-driven
	 * create/unlink/rename on the same directory.
	 *
	 * NOTE: if this fails the inode file remains on disk unreferenced.
	 * Orphaned inodes are cleaned up on the next `agfs commit` or
	 * `agfs abort` because the entire inode store is removed.
	 */
	inode_lock(d_inode(dentry->d_parent));
	de = agfs_add_dirent(d_inode(dentry->d_parent),
			      dentry->d_name.name,
			      dentry->d_name.len,
			      agfs_pde_inode(ino, (u16)atomic_read(&sbi->gen),
					    DT_REG, true));
	inode_unlock(d_inode(dentry->d_parent));
	if (IS_ERR(de)) {
		path_put(&inode_path);
		return PTR_ERR(de);
	}
	AGFS_D(dentry)->dirent = de;

	path_get(&inode_path); /* extra ref for reopen below */

	/* Update dentry lower_path to point at the inode (consumes original ref) */
	agfs_replace_lower_path(dentry, &inode_path);

	/* Append journal record (best-effort — dirent is already set) */
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
		list_del_init(&ii->de_pin);
		agfs_free_de_buckets(ii);
		iput(&ii->vfs_inode);
	}
}
