// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — append-only journal.
 *
 * Written by the kernel on every mutation. Read by the CLI for
 * commit/abort/status/diff. The kernel never reads it back.
 *
 * Record format (NUL-separated fields, newline-terminated):
 *   A\0<dir>\0<name>\0<dtype>\0<ino>\n       — add (new file)
 *   M\0<dir>\0<name>\0<dtype>\0<ino>\n       — modify (existing file)
 *   D\0<dir>\0<name>\n                        — delete
 *   R\0<dir>\0<name>\0<dtype>\0<base>\n       — redirect (rename)
 *   K\0<id>\0<name>\n                         — checkpoint marker
 *   S\0<gen>\0<target_gen>\n                  — restore
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

/* Compute dir_buf = relpath of dentry's parent. Returns 0 or -errno. */
static int journal_dir(struct dentry *dentry, char *dir_buf, size_t size)
{
	char *p;

	p = dentry_path_raw(dentry->d_parent, dir_buf, size);
	if (IS_ERR(p))
		return PTR_ERR(p);
	if (p != dir_buf)
		memmove(dir_buf, p, strlen(p) + 1);
	/* Root parent shows as "/" — normalize to "" */
	if (dir_buf[0] == '/' && dir_buf[1] == '\0')
		dir_buf[0] = '\0';
	return 0;
}

/* ── Public: typed journal record writers ──────────────────────────── */

static int journal_ino_record(struct agfs_sb_info *sbi, char tag,
			      struct dentry *dentry, u64 ino,
			      unsigned char d_type)
{
	char dir_buf[AGFS_PATH_MAX];
	char ino_str[21];
	char dtype_str[2] = { '\0', '\0' };
	int err;

	err = journal_dir(dentry, dir_buf, sizeof(dir_buf));
	if (err)
		return err;

	snprintf(ino_str, sizeof(ino_str), "%llu", (unsigned long long)ino);
	dtype_str[0] = dtype_to_char(d_type);

	return journal_write(sbi, tag,
			     (const char *[]){ dir_buf,
					       dentry->d_name.name,
					       dtype_str, ino_str,
					       NULL });
}

int agfs_journal_add(struct agfs_sb_info *sbi, struct dentry *dentry,
		     u64 ino, unsigned char d_type)
{
	return journal_ino_record(sbi, 'A', dentry, ino, d_type);
}

int agfs_journal_modify(struct agfs_sb_info *sbi, struct dentry *dentry,
			u64 ino, unsigned char d_type)
{
	return journal_ino_record(sbi, 'M', dentry, ino, d_type);
}

int agfs_journal_delete(struct agfs_sb_info *sbi, struct dentry *dentry)
{
	char dir_buf[AGFS_PATH_MAX];
	int err;

	err = journal_dir(dentry, dir_buf, sizeof(dir_buf));
	if (err)
		return err;

	return journal_write(sbi, 'D',
			     (const char *[]){ dir_buf,
					       dentry->d_name.name,
					       NULL });
}

int agfs_journal_redirect(struct agfs_sb_info *sbi, struct dentry *dentry,
			  unsigned char d_type, const char *base)
{
	char dir_buf[AGFS_PATH_MAX];
	char dtype_str[2] = { '\0', '\0' };
	int err;

	err = journal_dir(dentry, dir_buf, sizeof(dir_buf));
	if (err)
		return err;

	dtype_str[0] = dtype_to_char(d_type);

	return journal_write(sbi, 'R',
			     (const char *[]){ dir_buf,
					       dentry->d_name.name,
					       dtype_str, base,
					       NULL });
}

int agfs_journal_replace(struct agfs_sb_info *sbi, struct dentry *dentry,
			 unsigned char d_type, const char *base)
{
	char dir_buf[AGFS_PATH_MAX];
	char dtype_str[2] = { '\0', '\0' };
	int err;

	err = journal_dir(dentry, dir_buf, sizeof(dir_buf));
	if (err)
		return err;

	dtype_str[0] = dtype_to_char(d_type);

	return journal_write(sbi, 'P',
			     (const char *[]){ dir_buf,
					       dentry->d_name.name,
					       dtype_str, base,
					       NULL });
}

int agfs_journal_checkpoint(struct agfs_sb_info *sbi, u64 id, const char *name)
{
	char id_str[21];

	snprintf(id_str, sizeof(id_str), "%llu", (unsigned long long)id);
	return journal_write(sbi, 'K',
			     (const char *[]){ id_str, name, NULL });
}

/**
 * agfs_journal_restore - Append a restore record to the journal.
 * @sbi: superblock info (has journal_file)
 * @gen: new generation assigned to this restore
 * @target_gen: the checkpoint gen being restored to
 *
 * Format: S\0<gen>\0<target_gen>\n
 */
int agfs_journal_restore(struct agfs_sb_info *sbi, u64 gen, u64 target_gen)
{
	char gen_str[21];
	char target_str[21];

	snprintf(gen_str, sizeof(gen_str), "%llu", (unsigned long long)gen);
	snprintf(target_str, sizeof(target_str), "%llu",
		 (unsigned long long)target_gen);
	return journal_write(sbi, 'S',
			     (const char *[]){ gen_str, target_str, NULL });
}
