// SPDX-License-Identifier: GPL-2.0
/*
 * yolofs — append-only journal.
 *
 * Written by the kernel on every mutation. Read by the CLI for
 * commit/abort/status/diff. The kernel never reads it back.
 *
 * Record format (NUL-separated fields, newline-terminated):
 *   S\0<path>\0<ino>\n                — Stage (staged content at path)
 *   D\0<path>\n                       — Delete
 *   R\0<dst>\0<src>\n                  — Rename
 *   M\0<gen>\0<name>\n                — Mark
 *   J\0<gen>\0<target_gen>\n          — Jump
 *   B\0<path>\n                       — Blocked (permission denied;
 *                                       observational, does not set dirty)
 */

#include "yolofs.h"
#include <linux/fs.h>
#include <linux/file.h>
#include <linux/namei.h>

/* ── Open the journal file, cache on sbi ───────────────────────────── */

int yolo_journal_open(struct yolo_sb_info *sbi)
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

static int journal_write(struct yolo_sb_info *sbi, char tag,
			 const char **fields)
{
	char buf[3 * YOLO_PATH_MAX + 64];
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
	if (err >= 0 && tag != 'M' && tag != 'J' && tag != 'B')
		WRITE_ONCE(sbi->dirty, true);
	return err < 0 ? err : 0;
}

/* ── Helpers ───────────────────────────────────────────────────────── */

/* ── Public: typed journal record writers ──────────────────────────── */

int yolo_journal_stage(struct yolo_sb_info *sbi, struct dentry *dentry,
		      u32 ino)
{
	char path_buf[YOLO_PATH_MAX];
	char ino_str[11];
	char *path = dentry_path_raw(dentry, path_buf, sizeof(path_buf));
	if (IS_ERR(path))
		return PTR_ERR(path);

	snprintf(ino_str, sizeof(ino_str), "%u", ino);

	return journal_write(sbi, 'S',
			     (const char *[]){ path, ino_str, NULL });
}

int yolo_journal_delete(struct yolo_sb_info *sbi, struct dentry *dentry)
{
	char path_buf[YOLO_PATH_MAX];
	char *path = dentry_path_raw(dentry, path_buf, sizeof(path_buf));
	if (IS_ERR(path))
		return PTR_ERR(path);

	return journal_write(sbi, 'D',
			     (const char *[]){ path, NULL });
}

int yolo_journal_rename(struct yolo_sb_info *sbi, struct dentry *old_dentry,
			struct dentry *new_dentry)
{
	char dst_buf[YOLO_PATH_MAX];
	char src_buf[YOLO_PATH_MAX];
	char *dst_path, *src_path;

	dst_path = dentry_path_raw(new_dentry, dst_buf, sizeof(dst_buf));
	if (IS_ERR(dst_path))
		return PTR_ERR(dst_path);

	src_path = dentry_path_raw(old_dentry, src_buf, sizeof(src_buf));
	if (IS_ERR(src_path))
		return PTR_ERR(src_path);

	return journal_write(sbi, 'R',
			     (const char *[]){ dst_path, src_path, NULL });
}

int yolo_journal_mark(struct yolo_sb_info *sbi, u16 id, const char *name)
{
	char id_str[6];

	snprintf(id_str, sizeof(id_str), "%u", (unsigned)id);
	return journal_write(sbi, 'M',
			     (const char *[]){ id_str, name, NULL });
}

/**
 * yolo_journal_jump - Append a jump record to the journal.
 * @sbi: superblock info (has journal_file)
 * @gen: new generation assigned to this jump
 * @target_gen: the mark gen being jumped to
 *
 * Format: J\0<gen>\0<target_gen>\n
 */
int yolo_journal_jump(struct yolo_sb_info *sbi, u16 gen, u16 target_gen)
{
	char gen_str[6];
	char target_str[6];

	snprintf(gen_str, sizeof(gen_str), "%u", (unsigned)gen);
	snprintf(target_str, sizeof(target_str), "%u",
		 (unsigned)target_gen);
	return journal_write(sbi, 'J',
			     (const char *[]){ gen_str, target_str, NULL });
}

/**
 * yolo_journal_block - Append a "permission blocked" record to the journal.
 * @sbi: superblock info (has journal_file)
 * @dentry: the target dentry whose access was denied
 *
 * Observational record: written when a yolofs rule causes the kernel to
 * return -EACCES for an access. The path is the agent's intended target
 * (file for opens; child for parent-write-denied mutates), not the
 * parent dentry whose perm caused the denial. Does not set sbi->dirty
 * (see journal_write).
 *
 * Format: B\0<path>\n
 */
int yolo_journal_block(struct yolo_sb_info *sbi, struct dentry *dentry)
{
	char path_buf[YOLO_PATH_MAX];
	char *path = dentry_path_raw(dentry, path_buf, sizeof(path_buf));
	if (IS_ERR(path))
		return PTR_ERR(path);

	return journal_write(sbi, 'B',
			     (const char *[]){ path, NULL });
}
