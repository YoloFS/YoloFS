// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — staging layer helpers.
 *
 * Path resolution, copy-on-write, whiteout creation.
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

int agfs_staging_path(struct agfs_sb_info *sbi, const char *relpath,
		      struct path *result)
{
	if (!sbi->staging_dir.dentry)
		return -ENOENT;
	return resolve_subpath(&sbi->staging_dir, relpath, result);
}

int agfs_base_path(struct agfs_sb_info *sbi, const char *relpath,
		   struct path *result)
{
	return resolve_subpath(&sbi->base_path, relpath, result);
}

bool agfs_staging_has(struct agfs_sb_info *sbi, const char *relpath)
{
	struct path p;
	int err;

	if (!sbi->staging_dir.dentry)
		return false;

	err = agfs_staging_path(sbi, relpath, &p);
	if (err)
		return false;

	path_put(&p);
	return true;
}

bool agfs_is_whiteout(struct dentry *dentry)
{
	struct inode *inode;

	if (!dentry || d_is_negative(dentry))
		return false;

	inode = d_inode(dentry);
	return S_ISCHR(inode->i_mode) && inode->i_rdev == MKDEV(0, 0);
}

/* ── Whiteout Creation ─────────────────────────────────────────────── */

int agfs_create_whiteout(struct agfs_sb_info *sbi, const char *relpath)
{
	struct path parent_path;
	struct dentry *wh_dentry;
	struct inode *dir;
	char *parent, *name;
	char *buf;
	int err;

	if (!sbi->staging_dir.dentry)
		return -ENOENT;

	buf = kstrdup(relpath, GFP_KERNEL);
	if (!buf)
		return -ENOMEM;

	/* Split into parent directory and filename */
	name = strrchr(buf, '/');
	if (name) {
		*name = '\0';
		name++;
		parent = buf;
	} else {
		name = buf;
		parent = NULL;
	}

	/* Create parent dirs in staging */
	if (parent && *parent) {
		err = agfs_create_staging_parents(sbi, relpath);
		if (err)
			goto out;
	}

	/* Resolve parent in staging */
	if (parent && *parent)
		err = resolve_subpath(&sbi->staging_dir, parent, &parent_path);
	else
		err = resolve_subpath(&sbi->staging_dir, ".", &parent_path);
	if (err)
		goto out;

	dir = d_inode(parent_path.dentry);

	inode_lock(dir);
	wh_dentry = lookup_one_len(name, parent_path.dentry, strlen(name));
	if (IS_ERR(wh_dentry)) {
		err = PTR_ERR(wh_dentry);
		goto out_unlock;
	}

	/* Remove existing file/dir if present */
	if (d_is_positive(wh_dentry)) {
		if (d_is_dir(wh_dentry))
			err = vfs_rmdir(mnt_idmap(parent_path.mnt), dir, wh_dentry);
		else
			err = vfs_unlink(mnt_idmap(parent_path.mnt), dir, wh_dentry, NULL);
		dput(wh_dentry);
		if (err)
			goto out_unlock;
		wh_dentry = lookup_one_len(name, parent_path.dentry, strlen(name));
		if (IS_ERR(wh_dentry)) {
			err = PTR_ERR(wh_dentry);
			goto out_unlock;
		}
	}

	err = vfs_mknod(mnt_idmap(parent_path.mnt), dir, wh_dentry,
			S_IFCHR, MKDEV(0, 0));
	dput(wh_dentry);
out_unlock:
	inode_unlock(dir);
	path_put(&parent_path);
out:
	kfree(buf);
	return err;
}

/* ── Create Parent Directories in Staging ──────────────────────────── */

int agfs_create_staging_parents(struct agfs_sb_info *sbi, const char *relpath)
{
	char *buf, *p;
	struct path cur;
	int err = 0;

	if (!sbi->staging_dir.dentry)
		return -ENOENT;

	buf = kstrdup(relpath, GFP_KERNEL);
	if (!buf)
		return -ENOMEM;

	cur = sbi->staging_dir;
	path_get(&cur);

	/* Walk each component, creating dirs as needed */
	for (p = buf; *p; ) {
		struct path next;
		struct dentry *child;
		struct inode *dir;
		char *slash;

		/* Skip leading slashes */
		while (*p == '/')
			p++;
		if (!*p)
			break;

		slash = strchr(p, '/');
		if (!slash)
			break; /* last component is the file itself */
		*slash = '\0';

		/* Try to look up the component */
		err = vfs_path_lookup(cur.dentry, cur.mnt, p,
				      LOOKUP_DIRECTORY, &next);
		if (!err) {
			path_put(&cur);
			cur = next;
			p = slash + 1;
			continue;
		}

		/* Doesn't exist — create it */
		dir = d_inode(cur.dentry);
		inode_lock(dir);
		child = lookup_one_len(p, cur.dentry, strlen(p));
		if (IS_ERR(child)) {
			err = PTR_ERR(child);
			inode_unlock(dir);
			goto out;
		}
		if (d_is_negative(child)) {
			err = vfs_mkdir(mnt_idmap(cur.mnt), dir, child, 0755);
			if (err) {
				dput(child);
				inode_unlock(dir);
				goto out;
			}
		}
		dput(child);
		inode_unlock(dir);

		/* Now look it up properly */
		err = vfs_path_lookup(cur.dentry, cur.mnt, p,
				      LOOKUP_DIRECTORY, &next);
		if (err)
			goto out;

		path_put(&cur);
		cur = next;
		p = slash + 1;
	}

out:
	path_put(&cur);
	kfree(buf);
	return err;
}

/* ── Copy-on-Write ─────────────────────────────────────────────────── */

int agfs_do_cow(struct agfs_sb_info *sbi, const char *relpath,
		struct file **new_file, int flags)
{
	struct path base_p, staging_p;
	struct file *src, *dst;
	loff_t len;
	int err;

	err = agfs_create_staging_parents(sbi, relpath);
	if (err)
		return err;

	/* Open base file read-only */
	err = agfs_base_path(sbi, relpath, &base_p);
	if (err)
		return err;
	src = dentry_open(&base_p, O_RDONLY, current_cred());
	path_put(&base_p);
	if (IS_ERR(src))
		return PTR_ERR(src);

	/* Create and open staging file */
	/* First, create the staging file by looking up parent + vfs_create */
	{
		char *buf, *name, *parent;
		struct path parent_path;
		struct dentry *new_dentry;
		struct inode *dir;

		buf = kstrdup(relpath, GFP_KERNEL);
		if (!buf) {
			fput(src);
			return -ENOMEM;
		}

		name = strrchr(buf, '/');
		if (name) {
			*name = '\0';
			name++;
			parent = buf;
		} else {
			name = buf;
			parent = NULL;
		}

		if (parent && *parent)
			err = resolve_subpath(&sbi->staging_dir, parent,
					      &parent_path);
		else
			err = resolve_subpath(&sbi->staging_dir, ".",
					      &parent_path);
		if (err) {
			kfree(buf);
			fput(src);
			return err;
		}

		dir = d_inode(parent_path.dentry);
		inode_lock(dir);
		new_dentry = lookup_one_len(name, parent_path.dentry,
					    strlen(name));
		if (IS_ERR(new_dentry)) {
			err = PTR_ERR(new_dentry);
			inode_unlock(dir);
			path_put(&parent_path);
			kfree(buf);
			fput(src);
			return err;
		}
		if (d_is_negative(new_dentry)) {
			err = vfs_create(mnt_idmap(parent_path.mnt), dir,
					 new_dentry, 0644, true);
		}
		dput(new_dentry);
		inode_unlock(dir);
		path_put(&parent_path);
		kfree(buf);
		if (err) {
			fput(src);
			return err;
		}
	}

	/* Now open the staging file for writing */
	err = agfs_staging_path(sbi, relpath, &staging_p);
	if (err) {
		fput(src);
		return err;
	}
	dst = dentry_open(&staging_p, O_WRONLY | O_TRUNC, current_cred());
	path_put(&staging_p);
	if (IS_ERR(dst)) {
		fput(src);
		return PTR_ERR(dst);
	}

	/* Copy contents */
	len = i_size_read(file_inode(src));
	if (len > 0) {
		loff_t copied_total = 0;
		while (copied_total < len) {
			ssize_t copied;
			size_t chunk = min_t(loff_t, len - copied_total,
					     1 << 20);

			copied = vfs_copy_file_range(src, copied_total,
						     dst, copied_total,
						     chunk, 0);
			if (copied <= 0) {
				if (copied == 0)
					break;
				err = copied;
				goto out_close;
			}
			copied_total += copied;
		}
	}

	if (new_file) {
		fput(dst);
		/* Reopen with requested flags */
		err = agfs_staging_path(sbi, relpath, &staging_p);
		if (err) {
			fput(src);
			return err;
		}
		*new_file = dentry_open(&staging_p, flags, current_cred());
		path_put(&staging_p);
		if (IS_ERR(*new_file)) {
			err = PTR_ERR(*new_file);
			*new_file = NULL;
		}
	} else {
out_close:
		fput(dst);
	}
	fput(src);
	return err;
}

/* ── Create Empty Staging File ──────────────────────────────────────── */

int agfs_create_staging_empty(struct agfs_sb_info *sbi, const char *relpath,
			      struct file **new_file, int flags)
{
	struct path staging_p, parent_path;
	struct dentry *new_dentry;
	struct inode *dir;
	char *buf, *name, *parent;
	int err;

	err = agfs_create_staging_parents(sbi, relpath);
	if (err)
		return err;

	buf = kstrdup(relpath, GFP_KERNEL);
	if (!buf)
		return -ENOMEM;

	name = strrchr(buf, '/');
	if (name) {
		*name = '\0';
		name++;
		parent = buf;
	} else {
		name = buf;
		parent = NULL;
	}

	if (parent && *parent)
		err = resolve_subpath(&sbi->staging_dir, parent, &parent_path);
	else
		err = resolve_subpath(&sbi->staging_dir, ".", &parent_path);
	if (err) {
		kfree(buf);
		return err;
	}

	dir = d_inode(parent_path.dentry);
	inode_lock(dir);
	new_dentry = lookup_one_len(name, parent_path.dentry, strlen(name));
	if (IS_ERR(new_dentry)) {
		err = PTR_ERR(new_dentry);
		inode_unlock(dir);
		path_put(&parent_path);
		kfree(buf);
		return err;
	}
	if (d_is_negative(new_dentry)) {
		err = vfs_create(mnt_idmap(parent_path.mnt), dir,
				 new_dentry, 0644, true);
	}
	dput(new_dentry);
	inode_unlock(dir);
	path_put(&parent_path);
	kfree(buf);
	if (err)
		return err;

	/* Open the (now empty) staging file with requested flags */
	err = agfs_staging_path(sbi, relpath, &staging_p);
	if (err)
		return err;
	*new_file = dentry_open(&staging_p, flags, current_cred());
	path_put(&staging_p);
	if (IS_ERR(*new_file)) {
		err = PTR_ERR(*new_file);
		*new_file = NULL;
	}
	return err;
}

/* ── Resolve Lower Path (§3.4) ─────────────────────────────────────── */

int agfs_resolve_lower(struct dentry *dentry, struct path *result)
{
	struct agfs_sb_info *sbi = AGFS_SB(dentry->d_sb);
	struct agfs_dentry_info *di = AGFS_D(dentry);
	char buf[AGFS_PATH_MAX];
	int err;

	err = agfs_dentry_relpath(dentry, buf, sizeof(buf));
	if (err)
		return err;

	/* 1. Check staging */
	if (sbi->staging_dir.dentry && agfs_staging_has(sbi, buf)) {
		struct path staging;
		err = agfs_staging_path(sbi, buf, &staging);
		if (!err) {
			if (agfs_is_whiteout(staging.dentry)) {
				path_put(&staging);
				return -ENOENT;
			}
			*result = staging;
			return 0;
		}
	}

	/* 2. Check redirected lower_path (for journal) */
	if (di && di->lower_path.dentry) {
		*result = di->lower_path;
		path_get(result);
		return 0;
	}

	/* 3. Check base */
	err = agfs_base_path(sbi, buf, result);
	if (!err)
		return 0;

	return -ENOENT;
}

/* ── Append Rename Record ──────────────────────────────────────────── */

int agfs_append_rename(struct agfs_sb_info *sbi,
		       const char *old_path, const char *new_path)
{
	struct file *f;
	struct path journal_p;
	loff_t pos;
	ssize_t ret;
	size_t old_len = strlen(old_path) + 1; /* include \0 */
	size_t new_len = strlen(new_path) + 1;
	int err;

	/* Open or create the journal file */
	if (sbi->journal_path.dentry) {
		f = dentry_open(&sbi->journal_path,
				O_WRONLY | O_APPEND, current_cred());
	} else {
		/* Create the file */
		struct dentry *new_dentry;
		struct inode *dir;

		dir = d_inode(sbi->storage_path.dentry);
		inode_lock(dir);
		new_dentry = lookup_one_len("journal",
					    sbi->storage_path.dentry, 7);
		if (IS_ERR(new_dentry)) {
			inode_unlock(dir);
			return PTR_ERR(new_dentry);
		}
		if (d_is_negative(new_dentry)) {
			err = vfs_create(mnt_idmap(sbi->storage_path.mnt),
					 dir, new_dentry, 0644, true);
			if (err) {
				dput(new_dentry);
				inode_unlock(dir);
				return err;
			}
		}
		dput(new_dentry);
		inode_unlock(dir);

		/* Resolve and cache */
		err = vfs_path_lookup(sbi->storage_path.dentry,
				      sbi->storage_path.mnt,
				      "journal", 0, &journal_p);
		if (err)
			return err;
		sbi->journal_path = journal_p;

		f = dentry_open(&sbi->journal_path,
				O_WRONLY | O_APPEND, current_cred());
	}
	if (IS_ERR(f))
		return PTR_ERR(f);

	pos = f->f_pos;
	ret = kernel_write(f, old_path, old_len, &pos);
	if (ret < 0) {
		fput(f);
		return ret;
	}
	ret = kernel_write(f, new_path, new_len, &pos);
	fput(f);
	return ret < 0 ? ret : 0;
}
