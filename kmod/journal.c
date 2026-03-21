// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — append-only journal.
 *
 * Written by the kernel on every mutation. Read by the CLI for
 * commit/abort/status/diff. The kernel never reads it back.
 *
 * Record format (NUL-separated fields, newline-terminated):
 *   A\0<path>\0<dtype>\0<ino>\n       — Add (new path)
 *   M\0<path>\0<dtype>\0<ino>\n       — Modify (existing path)
 *   D\0<path>\0<dtype>\n              — Delete
 *   R\0<dst>\0<src>\0<dtype>\n         — Rename (destination is new)
 *   P\0<dst>\0<src>\0<dtype>\n         — Replace (destination existed in base)
 *   K\0<gen>\0<name>\n                — Checkpoint
 *   T\0<gen>\0<target_gen>\n          — Restore
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
	if (err >= 0 && tag != 'K' && tag != 'T')
		WRITE_ONCE(sbi->dirty, true);
	return err < 0 ? err : 0;
}

/* ── Helpers ───────────────────────────────────────────────────────── */

static char dtype_to_char(unsigned char d_type)
{
	switch (d_type) {
	case DT_DIR: return 'd';
	case DT_LNK: return 'l';
	case DT_REG: return 'f';
	default:     return '\0';
	}
}

/* ── Public: typed journal record writers ──────────────────────────── */

static int journal_emit_ino(struct agfs_sb_info *sbi,
			    struct dentry *dentry, u64 ino,
			    unsigned char d_type, char tag)
{
	char path_buf[AGFS_PATH_MAX];
	char ino_str[21];
	char dtype_str[2] = { '\0', '\0' };
	char *path = dentry_path_raw(dentry, path_buf, sizeof(path_buf));
	if (IS_ERR(path))
		return PTR_ERR(path);

	snprintf(ino_str, sizeof(ino_str), "%llu", (unsigned long long)ino);
	dtype_str[0] = dtype_to_char(d_type);

	return journal_write(sbi, tag,
			     (const char *[]){ path,
					       dtype_str, ino_str,
					       NULL });
}

int agfs_journal_add(struct agfs_sb_info *sbi, struct dentry *dentry,
		     u64 ino, unsigned char d_type)
{
	return journal_emit_ino(sbi, dentry, ino, d_type, 'A');
}

int agfs_journal_modify(struct agfs_sb_info *sbi, struct dentry *dentry,
			u64 ino, unsigned char d_type)
{
	return journal_emit_ino(sbi, dentry, ino, d_type, 'M');
}

int agfs_journal_delete(struct agfs_sb_info *sbi, struct dentry *dentry,
		       unsigned char d_type)
{
	char path_buf[AGFS_PATH_MAX];
	char dtype_str[2] = { '\0', '\0' };
	char *path = dentry_path_raw(dentry, path_buf, sizeof(path_buf));
	if (IS_ERR(path))
		return PTR_ERR(path);

	dtype_str[0] = dtype_to_char(d_type);

	return journal_write(sbi, 'D',
			     (const char *[]){ path,
					       dtype_str,
					       NULL });
}

static int journal_emit_paths(struct agfs_sb_info *sbi,
			       struct dentry *old_dentry,
			       struct dentry *new_dentry,
			       unsigned char d_type, char tag)
{
	char dst_buf[AGFS_PATH_MAX];
	char src_buf[AGFS_PATH_MAX];
	char dtype_str[2] = { '\0', '\0' };
	char *dst_path, *src_path;

	dst_path = dentry_path_raw(new_dentry, dst_buf, sizeof(dst_buf));
	if (IS_ERR(dst_path))
		return PTR_ERR(dst_path);

	src_path = dentry_path_raw(old_dentry, src_buf, sizeof(src_buf));
	if (IS_ERR(src_path))
		return PTR_ERR(src_path);

	dtype_str[0] = dtype_to_char(d_type);

	return journal_write(sbi, tag,
			     (const char *[]){ dst_path,
					       src_path,
					       dtype_str,
					       NULL });
}

int agfs_journal_rename(struct agfs_sb_info *sbi, struct dentry *old_dentry,
			struct dentry *new_dentry, unsigned char d_type)
{
	return journal_emit_paths(sbi, old_dentry, new_dentry, d_type, 'R');
}

int agfs_journal_replace(struct agfs_sb_info *sbi, struct dentry *old_dentry,
			 struct dentry *new_dentry, unsigned char d_type)
{
	return journal_emit_paths(sbi, old_dentry, new_dentry, d_type, 'P');
}

int agfs_journal_checkpoint(struct agfs_sb_info *sbi, u16 id, const char *name)
{
	char id_str[6];

	snprintf(id_str, sizeof(id_str), "%u", (unsigned)id);
	return journal_write(sbi, 'K',
			     (const char *[]){ id_str, name, NULL });
}

/**
 * agfs_journal_restore - Append a restore record to the journal.
 * @sbi: superblock info (has journal_file)
 * @gen: new generation assigned to this restore
 * @target_gen: the checkpoint gen being restored to
 *
 * Format: T\0<gen>\0<target_gen>\n
 */
int agfs_journal_restore(struct agfs_sb_info *sbi, u16 gen, u16 target_gen)
{
	char gen_str[6];
	char target_str[6];

	snprintf(gen_str, sizeof(gen_str), "%u", (unsigned)gen);
	snprintf(target_str, sizeof(target_str), "%u",
		 (unsigned)target_gen);
	return journal_write(sbi, 'T',
			     (const char *[]){ gen_str, target_str, NULL });
}
