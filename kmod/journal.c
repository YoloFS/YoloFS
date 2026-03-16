// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — append-only mutation journal.
 *
 * Written by the kernel on every mutation. Read by the CLI for
 * commit/abort/status/diff. The kernel never reads it back.
 *
 * Record format (NUL-separated fields, newline-terminated):
 *   A\0<path>\0<id>\n    — content/dir in staging/<id>
 *   D\0<path>\n          — deleted
 *   R\0<old>\0<new>\n    — rename hint
 *   S\0<id>\0<name>\n    — snapshot marker
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
	char buf[2 * AGFS_PATH_MAX + 32];
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

/* ── Public append helpers ─────────────────────────────────────────── */

int agfs_journal_append_a(struct agfs_sb_info *sbi, const char *path, u64 id)
{
	char id_str[21];

	snprintf(id_str, sizeof(id_str), "%llu", (unsigned long long)id);
	return journal_write(sbi, 'A',
			     (const char *[]){ path, id_str, NULL });
}

int agfs_journal_append_d(struct agfs_sb_info *sbi, const char *path)
{
	return journal_write(sbi, 'D',
			     (const char *[]){ path, NULL });
}

int agfs_journal_append_r(struct agfs_sb_info *sbi, const char *old_path,
			  const char *new_path)
{
	return journal_write(sbi, 'R',
			     (const char *[]){ old_path, new_path, NULL });
}

int agfs_journal_append_s(struct agfs_sb_info *sbi, u64 id, const char *name)
{
	char id_str[21];

	snprintf(id_str, sizeof(id_str), "%llu", (unsigned long long)id);
	return journal_write(sbi, 'S',
			     (const char *[]){ id_str, name, NULL });
}
