// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — append-only journal.
 *
 * Written by the kernel on every mutation. Read by the CLI for
 * commit/abort/status/diff. The kernel never reads it back.
 *
 * Record format (NUL-separated fields, newline-terminated):
 *   E\0<dir>\0<name>\0<ino>\0<dtype>\0<base>\n    — entry (staged/deleted/redirect)
 *   K\0<id>\0<name>\n                              — checkpoint marker
 */

#include "agfs.h"
#include <linux/fs.h>
#include <linux/file.h>
#include <linux/namei.h>

/* ── Open the journal file, cache on sbi ───────────────────────────── */

int agfs_journal_open(struct agfs_sb_info *sbi)
{
	struct path journal_p;
	struct file *f;
	int err;

	err = vfs_path_lookup(sbi->storage_path.dentry,
			      sbi->storage_path.mnt,
			      "journal", 0, &journal_p);
	if (err)
		return err;

	f = dentry_open(&journal_p, O_WRONLY | O_APPEND, current_cred());
	path_put(&journal_p);
	if (IS_ERR(f))
		return PTR_ERR(f);

	sbi->journal_file = f;
	return 0;
}

/* ── Write one record ───────────────────────────────────────────────── */

static int journal_write(struct agfs_sb_info *sbi, char tag,
			 const char **fields)
{
	char buf[3 * AGFS_PATH_MAX + 64];
	size_t off = 0;
	const char **fp;
	struct file *f = sbi->journal_file;
	loff_t pos;
	int err;

	if (!f)
		return -EIO;

	buf[off++] = tag;
	buf[off++] = '\0';
	for (fp = fields; *fp; fp++) {
		size_t len = strlen(*fp);

		if (off + len + 1 > sizeof(buf))
			return -ENAMETOOLONG;
		memcpy(buf + off, *fp, len);
		off += len;
		buf[off++] = (*(fp + 1)) ? '\0' : '\n';
	}

	pos = f->f_pos;
	err = kernel_write(f, buf, off, &pos);
	return err < 0 ? err : 0;
}

/* ── Public helpers ────────────────────────────────────────────────── */

static char dtype_to_char(unsigned char d_type)
{
	switch (d_type) {
	case DT_DIR: return 'd';
	case DT_LNK: return 'l';
	case DT_REG: return 'f';
	default:     return '\0';
	}
}

int agfs_journal_append(struct agfs_sb_info *sbi, struct dentry *dentry,
			u64 ino, unsigned char d_type, const char *base)
{
	char dir_buf[AGFS_PATH_MAX];
	char ino_str[21];
	char dtype_str[2] = { '\0', '\0' };
	char *p;

	/* dir = relpath of parent */
	p = dentry_path_raw(dentry->d_parent, dir_buf, sizeof(dir_buf));
	if (IS_ERR(p))
		return PTR_ERR(p);
	if (p != dir_buf)
		memmove(dir_buf, p, strlen(p) + 1);
	/* Root parent shows as "/" — normalize to "" */
	if (dir_buf[0] == '/' && dir_buf[1] == '\0')
		dir_buf[0] = '\0';

	if (ino == AGFS_INO_REDIRECT)
		snprintf(ino_str, sizeof(ino_str), "-1");
	else
		snprintf(ino_str, sizeof(ino_str), "%llu",
			 (unsigned long long)ino);

	dtype_str[0] = dtype_to_char(d_type);

	return journal_write(sbi, 'E',
			     (const char *[]){ dir_buf,
					       dentry->d_name.name,
					       ino_str, dtype_str,
					       base ? base : "",
					       NULL });
}

int agfs_journal_checkpoint(struct agfs_sb_info *sbi, u64 id, const char *name)
{
	char id_str[21];

	snprintf(id_str, sizeof(id_str), "%llu", (unsigned long long)id);
	return journal_write(sbi, 'K',
			     (const char *[]){ id_str, name, NULL });
}
