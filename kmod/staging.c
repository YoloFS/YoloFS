// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — staging layer helpers.
 *
 * Flat inode store, override hash table management, COW.
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

/* ── Override Hash Table ───────────────────────────────────────────── */

static inline unsigned int agfs_ovr_hash(const char *name, unsigned int len)
{
	return full_name_hash(NULL, name, len) >> (32 - AGFS_OVR_SHIFT);
}

/*
 * Find an override by name. Caller must hold dii->ovr_lock.
 */
struct agfs_override *agfs_find_override(struct inode *dir,
					 const char *name,
					 unsigned int namelen)
{
	struct agfs_inode_info *dii = AGFS_I(dir);
	struct agfs_override *ovr;
	unsigned int idx;

	if (!dii->ovr_buckets)
		return NULL;

	idx = agfs_ovr_hash(name, namelen);
	hlist_for_each_entry(ovr, &dii->ovr_buckets[idx], node) {
		if (ovr->name_len == namelen &&
		    !memcmp(ovr->name, name, namelen))
			return ovr;
	}
	return NULL;
}

/*
 * Free all override entries and the bucket array on a directory inode.
 */
static void agfs_free_ovr_buckets(struct agfs_inode_info *dii)
{
	struct hlist_head *buckets;
	struct agfs_override *ovr;
	struct hlist_node *tmp;
	unsigned int i;

	spin_lock(&dii->ovr_lock);
	buckets = dii->ovr_buckets;
	dii->ovr_buckets = NULL;
	spin_unlock(&dii->ovr_lock);

	if (!buckets)
		return;

	for (i = 0; i < AGFS_OVR_BUCKETS; i++) {
		hlist_for_each_entry_safe(ovr, tmp, &buckets[i], node) {
			hlist_del(&ovr->node);
			kfree(ovr->base_path);
			kfree(ovr);
		}
	}
	kfree(buckets);
}

/*
 * Add or update an override. ino=0 && base_path=NULL means deleted.
 * On first override for a directory, pins the inode via igrab().
 */
int agfs_add_override(struct inode *dir, const char *name,
		      unsigned int namelen, u64 ino,
		      const char *base_path, unsigned char d_type,
		      u64 snapshot_gen)
{
	struct agfs_inode_info *dii = AGFS_I(dir);
	struct agfs_sb_info *sbi = AGFS_SB(dir->i_sb);
	struct agfs_override *ovr, *new_ovr = NULL;
	struct hlist_head *new_buckets = NULL;
	char *bp_copy = NULL;
	unsigned int i;
	bool first_override = false;

	/* Pre-allocate outside the lock (GFP_KERNEL is safe here) */
	new_ovr = kmalloc(offsetof(struct agfs_override, name) + namelen + 1,
			  GFP_KERNEL);
	if (!new_ovr)
		return -ENOMEM;

	if (base_path) {
		bp_copy = kstrdup(base_path, GFP_KERNEL);
		if (!bp_copy) {
			kfree(new_ovr);
			return -ENOMEM;
		}
	}

	if (!dii->ovr_buckets) {
		new_buckets = kmalloc_array(AGFS_OVR_BUCKETS,
					    sizeof(struct hlist_head),
					    GFP_KERNEL);
		if (!new_buckets) {
			kfree(bp_copy);
			kfree(new_ovr);
			return -ENOMEM;
		}
		for (i = 0; i < AGFS_OVR_BUCKETS; i++)
			INIT_HLIST_HEAD(&new_buckets[i]);
	}

	spin_lock(&dii->ovr_lock);

	/* Install bucket array if we're the first adder */
	if (!dii->ovr_buckets && new_buckets) {
		dii->ovr_buckets = new_buckets;
		new_buckets = NULL;	/* ownership transferred */
		first_override = true;
	}

	ovr = agfs_find_override(dir, name, namelen);
	if (ovr) {
		/* Update existing */
		kfree(ovr->base_path);
		ovr->ino = ino;
		ovr->base_path = bp_copy;
		ovr->d_type = d_type;
		ovr->snapshot_gen = snapshot_gen;
		spin_unlock(&dii->ovr_lock);
		kfree(new_ovr);
		kfree(new_buckets);
	} else {
		/* Insert new */
		memcpy(new_ovr->name, name, namelen);
		new_ovr->name[namelen] = '\0';
		new_ovr->name_len = namelen;
		new_ovr->ino = ino;
		new_ovr->base_path = bp_copy;
		new_ovr->d_type = d_type;
		new_ovr->snapshot_gen = snapshot_gen;
		hlist_add_head(&new_ovr->node,
			       &dii->ovr_buckets[agfs_ovr_hash(name, namelen)]);
		spin_unlock(&dii->ovr_lock);
		kfree(new_buckets);
	}

	/* Pin the directory inode on first override */
	if (first_override) {
		if (!igrab(&dii->vfs_inode)) {
			agfs_free_ovr_buckets(dii);
			return -EIO;
		}
		spin_lock(&sbi->pinned_dirs_lock);
		list_add(&dii->ovr_pin, &sbi->pinned_dirs);
		spin_unlock(&sbi->pinned_dirs_lock);
	}
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

/* ── Copy-on-Write to Inode Store ──────────────────────────────────── */

int agfs_do_cow(struct agfs_sb_info *sbi, struct dentry *dentry,
		struct file **new_file, int flags, bool truncate)
{
	struct path inode_path;
	u64 ino;
	int err;

	/* Allocate a new inode in the store, preserving the base file's mode */
	err = agfs_inode_alloc(sbi, &ino, &inode_path,
			       d_inode(dentry)->i_mode & ~S_IFMT, NULL);
	if (err)
		return err;

	/* Copy base content to inode (skip when truncating — inode stays empty) */
	if (!truncate) {
		struct file *src, *dst;
		struct path lower_path;
		loff_t len;

		agfs_get_lower_path(dentry, &lower_path);
		src = dentry_open(&lower_path, O_RDONLY, current_cred());
		agfs_put_lower_path(dentry, &lower_path);
		if (IS_ERR(src)) {
			path_put(&inode_path);
			return PTR_ERR(src);
		}

		dst = dentry_open(&inode_path, O_WRONLY | O_TRUNC,
				  current_cred());
		if (IS_ERR(dst)) {
			fput(src);
			path_put(&inode_path);
			return PTR_ERR(dst);
		}

		len = i_size_read(file_inode(src));
		if (len > 0) {
			loff_t copied_total = 0;

			while (copied_total < len) {
				ssize_t copied;
				size_t chunk = min_t(loff_t,
						     len - copied_total,
						     1 << 20);

				copied = vfs_copy_file_range(src, copied_total,
							     dst, copied_total,
							     chunk, 0);
				if (copied <= 0) {
					if (copied == 0)
						break;
					fput(dst);
					fput(src);
					path_put(&inode_path);
					return copied;
				}
				copied_total += copied;
			}
		}
		fput(dst);
		fput(src);
	}

	/*
	 * Add override on parent directory inode.
	 *
	 * NOTE: if this fails the inode file remains on disk unreferenced.
	 * Orphaned inodes are cleaned up on the next `agfs commit` or
	 * `agfs abort` because the entire inode store is removed.
	 */
	err = agfs_add_override(d_inode(dentry->d_parent),
				dentry->d_name.name,
				dentry->d_name.len,
				ino, NULL, DT_REG,
				(u64)atomic64_read(&sbi->snapshot_gen));
	if (err) {
		path_put(&inode_path);
		return err;
	}

	path_get(&inode_path); /* extra ref for reopen below */

	/* Update dentry lower_path to point at the inode (consumes original ref) */
	agfs_replace_lower_path(dentry, &inode_path);

	/* Append journal record (best-effort — override is already set) */
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

	list_for_each_entry_safe(ii, tmp, &local, ovr_pin) {
		list_del_init(&ii->ovr_pin);
		agfs_free_ovr_buckets(ii);
		iput(&ii->vfs_inode);
	}
}
