// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — append-only journal.
 *
 * Written by the kernel on every mutation. Read by the CLI for
 * commit/abort/status/diff. The kernel never reads it back.
 *
 * Record format (NUL-separated fields, newline-terminated):
 *   S\0<path>\0<dtype>\0<ino>\n       — Stage (staged content at path)
 *   D\0<path>\0<dtype>\n              — Delete
 *   R\0<dst>\0<src>\0<dtype>\n         — Rename
 *   M\0<gen>\0<name>\n                — Mark
 *   J\0<gen>\0<target_gen>\n          — Jump
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
	if (err >= 0 && tag != 'M' && tag != 'J')
		WRITE_ONCE(sbi->dirty, true);
	return err < 0 ? err : 0;
}

/* ── Helpers ───────────────────────────────────────────────────────── */

/* ── Public: typed journal record writers ──────────────────────────── */

int agfs_journal_stage(struct agfs_sb_info *sbi, struct dentry *dentry,
		      u32 ino, unsigned char d_type)
{
	char path_buf[AGFS_PATH_MAX];
	char ino_str[11];
	char dtype_str[4];
	char *path = dentry_path_raw(dentry, path_buf, sizeof(path_buf));
	if (IS_ERR(path))
		return PTR_ERR(path);

	snprintf(ino_str, sizeof(ino_str), "%u", ino);
	snprintf(dtype_str, sizeof(dtype_str), "%u", (unsigned)d_type);

	return journal_write(sbi, 'S',
			     (const char *[]){ path,
					       dtype_str, ino_str,
					       NULL });
}

int agfs_journal_delete(struct agfs_sb_info *sbi, struct dentry *dentry,
		       unsigned char d_type)
{
	char path_buf[AGFS_PATH_MAX];
	char dtype_str[4];
	char *path = dentry_path_raw(dentry, path_buf, sizeof(path_buf));
	if (IS_ERR(path))
		return PTR_ERR(path);

	snprintf(dtype_str, sizeof(dtype_str), "%u", (unsigned)d_type);

	return journal_write(sbi, 'D',
			     (const char *[]){ path,
					       dtype_str,
					       NULL });
}

int agfs_journal_rename(struct agfs_sb_info *sbi, struct dentry *old_dentry,
			struct dentry *new_dentry, unsigned char d_type)
{
	char dst_buf[AGFS_PATH_MAX];
	char src_buf[AGFS_PATH_MAX];
	char dtype_str[4];
	char *dst_path, *src_path;

	dst_path = dentry_path_raw(new_dentry, dst_buf, sizeof(dst_buf));
	if (IS_ERR(dst_path))
		return PTR_ERR(dst_path);

	src_path = dentry_path_raw(old_dentry, src_buf, sizeof(src_buf));
	if (IS_ERR(src_path))
		return PTR_ERR(src_path);

	snprintf(dtype_str, sizeof(dtype_str), "%u", (unsigned)d_type);

	return journal_write(sbi, 'R',
			     (const char *[]){ dst_path,
					       src_path,
					       dtype_str,
					       NULL });
}

int agfs_journal_mark(struct agfs_sb_info *sbi, u16 id, const char *name)
{
	char id_str[6];

	snprintf(id_str, sizeof(id_str), "%u", (unsigned)id);
	return journal_write(sbi, 'M',
			     (const char *[]){ id_str, name, NULL });
}

/**
 * agfs_journal_jump - Append a jump record to the journal.
 * @sbi: superblock info (has journal_file)
 * @gen: new generation assigned to this jump
 * @target_gen: the mark gen being jumped to
 *
 * Format: J\0<gen>\0<target_gen>\n
 */
int agfs_journal_jump(struct agfs_sb_info *sbi, u16 gen, u16 target_gen)
{
	char gen_str[6];
	char target_str[6];

	snprintf(gen_str, sizeof(gen_str), "%u", (unsigned)gen);
	snprintf(target_str, sizeof(target_str), "%u",
		 (unsigned)target_gen);
	return journal_write(sbi, 'J',
			     (const char *[]){ gen_str, target_str, NULL });
}
