// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — staging layer helpers.
 *
 * Flat blob store, override hash table management, COW.
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

int agfs_staging_path(struct agfs_sb_info *sbi, u64 id,
			   struct path *result)
{
	char name[21];

	if (!sbi->staging_dir.dentry)
		return -ENOENT;

	snprintf(name, sizeof(name), "%llu", (unsigned long long)id);
	return resolve_subpath(&sbi->staging_dir, name, result);
}

/* ── Override Hash Table (§3.4) ─────────────────────────────────────── */

static inline unsigned int agfs_ovr_hash(const char *name, unsigned int len)
{
	return full_name_hash(NULL, name, len) >> (32 - AGFS_OVR_SHIFT);
}

/*
 * Find an override by name. Caller must hold di->lock.
 */
struct agfs_override *agfs_find_override(struct dentry *dir_dentry,
					 const char *name,
					 unsigned int namelen)
{
	struct agfs_dentry_info *di = AGFS_D(dir_dentry);
	struct agfs_override *ovr;
	unsigned int idx;

	if (!di || !di->ovr_buckets)
		return NULL;

	idx = agfs_ovr_hash(name, namelen);
	hlist_for_each_entry(ovr, &di->ovr_buckets[idx], node) {
		if (ovr->name_len == namelen &&
		    !memcmp(ovr->name, name, namelen))
			return ovr;
	}
	return NULL;
}

/*
 * Add or update an override. staging_id=0 && base_path=NULL means deleted.
 */
int agfs_add_override(struct dentry *dir_dentry, const char *name,
		      unsigned int namelen, u64 staging_id,
		      const char *base_path, unsigned char d_type)
{
	struct agfs_dentry_info *di = AGFS_D(dir_dentry);
	struct agfs_override *ovr, *new_ovr = NULL;
	struct hlist_head *new_buckets = NULL;
	char *bp_copy = NULL;
	unsigned int i;

	if (!di)
		return -EINVAL;

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

	if (!di->ovr_buckets) {
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

	spin_lock(&di->lock);

	/* Install bucket array if we're the first adder */
	if (!di->ovr_buckets && new_buckets) {
		di->ovr_buckets = new_buckets;
		new_buckets = NULL;	/* ownership transferred */
	}

	ovr = agfs_find_override(dir_dentry, name, namelen);
	if (ovr) {
		/* Update existing */
		kfree(ovr->base_path);
		ovr->staging_id = staging_id;
		ovr->base_path = bp_copy;
		ovr->d_type = d_type;
		spin_unlock(&di->lock);
		kfree(new_ovr);
		kfree(new_buckets);
		return 0;
	}

	/* Insert new */
	memcpy(new_ovr->name, name, namelen);
	new_ovr->name[namelen] = '\0';
	new_ovr->name_len = namelen;
	new_ovr->staging_id = staging_id;
	new_ovr->base_path = bp_copy;
	new_ovr->d_type = d_type;
	hlist_add_head(&new_ovr->node,
		       &di->ovr_buckets[agfs_ovr_hash(name, namelen)]);
	spin_unlock(&di->lock);
	kfree(new_buckets);
	return 0;
}

/* ── Staging Blob Allocation ───────────────────────────────────────── */

/*
 * Allocate a new staging ID, look up and create the blob.
 * Regular files get vfs_create; dirs get vfs_mkdir; symlinks get vfs_symlink.
 *
 * @mode: S_IFREG for regular file, S_IFDIR for directory, S_IFLNK for symlink.
 *        Lower bits are the permission mode (used for dirs).
 * @symname: symlink target (only for S_IFLNK, NULL otherwise).
 */
int agfs_staging_alloc(struct agfs_sb_info *sbi, u64 *out_id,
		       struct path *blob_path, umode_t mode,
		       const char *symname)
{
	char name[21];
	struct dentry *blob_dentry;
	struct inode *dir;
	u64 id;
	int err;

	if (!sbi->staging_dir.dentry)
		return -ENOENT;

	id = atomic64_inc_return(&sbi->next_staging_id);
	snprintf(name, sizeof(name), "%llu", (unsigned long long)id);

	dir = d_inode(sbi->staging_dir.dentry);
	inode_lock(dir);
	blob_dentry = lookup_one_len(name, sbi->staging_dir.dentry,
				     strlen(name));
	if (IS_ERR(blob_dentry)) {
		inode_unlock(dir);
		return PTR_ERR(blob_dentry);
	}

	if (S_ISDIR(mode))
		err = vfs_mkdir(mnt_idmap(sbi->staging_dir.mnt),
				dir, blob_dentry, mode);
	else if (S_ISLNK(mode))
		err = vfs_symlink(mnt_idmap(sbi->staging_dir.mnt),
				  dir, blob_dentry, symname);
	else
		err = vfs_create(mnt_idmap(sbi->staging_dir.mnt),
				 dir, blob_dentry, mode, true);

	inode_unlock(dir);
	if (err) {
		dput(blob_dentry);
		return err;
	}

	blob_path->dentry = blob_dentry;
	blob_path->mnt = mntget(sbi->staging_dir.mnt);
	*out_id = id;
	return 0;
}

/* ── Copy-on-Write to Staging Blob ─────────────────────────────────── */

int agfs_do_cow(struct agfs_sb_info *sbi, struct dentry *dentry,
		     struct file **new_file, int flags, bool truncate)
{
	struct path blob_path;
	u64 id;
	int err;

	/* Allocate a new staging blob */
	err = agfs_staging_alloc(sbi, &id, &blob_path, 0644, NULL);
	if (err)
		return err;

	/* Copy base content to blob (skip when truncating — blob stays empty) */
	if (!truncate) {
		struct file *src, *dst;
		struct path lower_path;
		loff_t len;

		agfs_get_lower_path(dentry, &lower_path);
		src = dentry_open(&lower_path, O_RDONLY, current_cred());
		agfs_put_lower_path(dentry, &lower_path);
		if (IS_ERR(src)) {
			path_put(&blob_path);
			return PTR_ERR(src);
		}

		dst = dentry_open(&blob_path, O_WRONLY | O_TRUNC,
				  current_cred());
		if (IS_ERR(dst)) {
			fput(src);
			path_put(&blob_path);
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
					path_put(&blob_path);
					return copied;
				}
				copied_total += copied;
			}
		}
		fput(dst);
		fput(src);
	}

	/*
	 * Add override on parent directory.
	 *
	 * NOTE: if this fails the blob file remains on disk unreferenced
	 * (the journal was not written yet).  This is a known minor leak;
	 * orphaned blobs are cleaned up on the next `agfs commit` or
	 * `agfs abort` because the entire staging directory is removed.
	 */
	err = agfs_add_override(dentry->d_parent,
				dentry->d_name.name,
				dentry->d_name.len,
				id, NULL, DT_REG);
	if (err) {
		path_put(&blob_path);
		return err;
	}

	/* Update dentry lower_path to point at the blob */
	agfs_set_lower_path(dentry, &blob_path);

	/* Track COW generation on inode for new handle initialization */
	AGFS_I(d_inode(dentry))->snapshot_gen =
		atomic64_read(&sbi->snapshot_gen);

	/* Append journal record (best-effort — override is already set) */
	{
		char buf[AGFS_PATH_MAX];

		if (!agfs_dentry_relpath(dentry, buf, sizeof(buf)))
			agfs_journal_append_a(sbi, buf, id);
	}

	/* Reopen with requested flags */
	err = 0;
	if (new_file) {
		struct path reopen;

		err = agfs_staging_path(sbi, id, &reopen);
		if (!err) {
			*new_file = dentry_open(&reopen, flags,
						current_cred());
			path_put(&reopen);
			if (IS_ERR(*new_file)) {
				err = PTR_ERR(*new_file);
				*new_file = NULL;
			}
		}
	}
	return err;
}
