// SPDX-License-Identifier: GPL-2.0
/*
 * yolofs — append-only journal.
 *
 * Written by the kernel for mutations, control markers, and audit events.
 * Read by the CLI for commit/abort/review/diff/journal. The kernel never reads
 * it back.
 *
 * Record format (NUL-separated fields, newline-terminated):
 *   S\0<path>\0<ino>\0<pre>\n          — Stage (post = StagedFile(ino))
 *   D\0<path>\0<pre>\n                 — Delete (post = None)
 *   R\0<dst>\0<src>\0<src_pre>\0<dst_pre>\n  — Rename
 *   P\0<name>\n                       — Snapshot
 *   T\0<target_gen>\n                 — Travel
 *   G\0<path>\0<op>\0<result>\n        — Prompted or denied access (observational)
 *   C\0<path>\0<policy>\n           — Live policy configuration (observational)
 *   (op = r/w; result = d/y/n; policy = q/a/w/r/d/h/u)
 *
 * Record tags are uppercase. Each *pre field is the operation-local pre-op
 * backing of that overlay name, tagged with the lowercased first letter of the
 * userspace Backing variant: "a" (None), "s:<ino>" (StagedFile), "b:<abspath>"
 * (BasePath). See yolo_preimage_backing() and docs/staging.md.
 */

#include "yolofs.h"
#include <linux/fs.h>
#include <linux/file.h>
#include <linux/namei.h>

/* ── Open the journal file for append ──────────────────────────────── */

/* Open <storage>/journal O_WRONLY|O_APPEND. Returns the file on success or an
 * ERR_PTR; the caller owns the returned file and caches it where it wants. */
struct file *yolo_journal_open(const struct path *storage)
{
	struct path journal_p;
	struct file *f;
	int err;

	err = vfs_path_lookup(storage->dentry, storage->mnt,
			      "journal", 0, &journal_p);
	if (err)
		return ERR_PTR(err);

	f = dentry_open(&journal_p, O_WRONLY | O_APPEND, current_cred());
	path_put(&journal_p);
	return f;
}

/* ── Write one record ───────────────────────────────────────────────── */

static int journal_write(struct yolo_sb_info *sbi, char tag,
			 const char **fields)
{
	char *buf;
	size_t bufsz = 2;
	size_t off = 0;
	const char **fp;
	struct file *f = sbi->staging.journal_file;
	loff_t pos;
	ssize_t written;

	if (!f)
		return -EIO;

	for (fp = fields; *fp; fp++) {
		size_t len = strlen(*fp);

		if (len > SIZE_MAX - bufsz - 1)
			return -EOVERFLOW;
		bufsz += len + 1;
	}

	buf = kmalloc(bufsz, GFP_KERNEL);
	if (!buf)
		return -ENOMEM;

	buf[off++] = tag;
	buf[off++] = '\0';
	for (fp = fields; *fp; fp++) {
		size_t len = strlen(*fp);

		memcpy(buf + off, *fp, len);
		off += len;
		buf[off++] = (*(fp + 1)) ? '\0' : '\n';
	}

	pos = f->f_pos;
	written = kernel_write(f, buf, off, &pos);
	kfree(buf);
	if (written < 0)
		return written;
	if (written != off)
		return -EIO;

	/* Only data mutations (S/D/R) mark the session dirty. Markers (P/T) and
	 * observational notes (G/C) are excluded — they must not trigger an
	 * auto-snapshot under YOLO_SNAPSHOT_IF_CHANGED. */
	if (tag == 'S' || tag == 'D' || tag == 'R')
		WRITE_ONCE(sbi->staging.dirty, true);
	return 0;
}

/* ── Helpers ───────────────────────────────────────────────────────── */

/* Tagged operation-local pre-image backing of @dentry, written into @buf. The
 * tag is the lowercased first letter of the userspace `Backing` variant it
 * parses to, so the pre namespace never shares letters with the record tags:
 *   "a"            None: negative dentry / tombstone / unresolvable
 *   "s:<ino>"      StagedFile: staged inode (backing == YOLO_BACKING_STAGED)
 *   "b:<abspath>"  BasePath: redirect-resolved base content (PATH or ground)
 * This is the exact pre-op backing — an already-staged file reports its staged
 * inode (s:), not the base it was COW'd from. The CLI parses this into a
 * `Backing` to seed a review range's old side; see docs/staging.md. */
const char *yolo_preimage_backing(const struct dentry *dentry,
				 char *buf, int len)
{
	struct yolo_dentry_info *di = YOLO_D(dentry);
	struct path lower;
	char *p;

	if (d_is_negative(dentry) || di->backing == YOLO_BACKING_NONE)
		return "a";

	if (di->backing == YOLO_BACKING_STAGED) {
		u32 ino = YOLO_I(d_inode(dentry))->staging_ino;

		if (!ino)
			return "a";
		snprintf(buf, len, "s:%u", ino);
		return buf;
	}

	/* PATH or ground state: redirect-resolved base path, tagged "b:". Write
	 * the path right-aligned into buf+2.. and prepend the tag just before
	 * the returned pointer (d_path returns a right-aligned slice). */
	yolo_get_lower_path(dentry, &lower);
	p = d_path(&lower, buf + 2, len - 2);
	yolo_put_lower_path(dentry, &lower);
	if (IS_ERR(p))
		return "a";
	p[-2] = 'b';
	p[-1] = ':';
	return p - 2;
}

/* ── Public: typed journal record writers ──────────────────────────── */

int yolo_journal_stage(struct yolo_sb_info *sbi, struct dentry *dentry,
		      u32 ino, const char *pre)
{
	char path_buf[YOLO_PATH_MAX];
	char ino_str[11];
	char *path = dentry_path_raw(dentry, path_buf, sizeof(path_buf));
	if (IS_ERR(path))
		return PTR_ERR(path);

	snprintf(ino_str, sizeof(ino_str), "%u", ino);

	/* `pre` is the tagged pre-op backing (a / s:<ino> / b:<path>), captured by
	 * the caller before the lower_path swap. The post-backing is StagedFile(ino). */
	return journal_write(sbi, 'S',
			     (const char *[]){ path, ino_str,
					       pre ? pre : "a", NULL });
}

int yolo_journal_delete(struct yolo_sb_info *sbi, struct dentry *dentry)
{
	char path_buf[YOLO_PATH_MAX];
	char pre_buf[YOLO_PATH_MAX];
	char *path = dentry_path_raw(dentry, path_buf, sizeof(path_buf));
	if (IS_ERR(path))
		return PTR_ERR(path);

	/* The dentry is still positive here (journal is before d_drop), so its
	 * pre-op backing is the content being removed. */
	return journal_write(sbi, 'D',
			     (const char *[]){ path,
					       yolo_preimage_backing(dentry, pre_buf,
								    sizeof(pre_buf)),
					       NULL });
}

int yolo_journal_rename(struct yolo_sb_info *sbi, struct dentry *old_dentry,
			struct dentry *new_dentry)
{
	char *bufs, *dst_buf, *src_buf, *src_pre_buf, *dst_pre_buf;
	char *dst_path, *src_path;
	const char *src_pre, *dst_pre;
	int err;

	/* Four path-sized scratch buffers, off the stack as one block. */
	bufs = kmalloc_array(4, YOLO_PATH_MAX, GFP_KERNEL);
	if (!bufs)
		return -ENOMEM;
	dst_buf = bufs;
	src_buf = bufs + YOLO_PATH_MAX;
	src_pre_buf = bufs + 2 * YOLO_PATH_MAX;
	dst_pre_buf = bufs + 3 * YOLO_PATH_MAX;

	dst_path = dentry_path_raw(new_dentry, dst_buf, YOLO_PATH_MAX);
	src_path = dentry_path_raw(old_dentry, src_buf, YOLO_PATH_MAX);
	if (IS_ERR(dst_path) || IS_ERR(src_path)) {
		err = IS_ERR(dst_path) ? PTR_ERR(dst_path) : PTR_ERR(src_path);
		kfree(bufs);
		return err;
	}

	/* Capture both pre-op backings before any dentry state change (the caller
	 * journals before d_move and the pin updates). A negative destination
	 * (fresh name or pinned tombstone) has no backing → "a". */
	src_pre = yolo_preimage_backing(old_dentry, src_pre_buf, YOLO_PATH_MAX);
	dst_pre = d_is_positive(new_dentry)
		? yolo_preimage_backing(new_dentry, dst_pre_buf, YOLO_PATH_MAX)
		: "a";

	err = journal_write(sbi, 'R',
			    (const char *[]){ dst_path, src_path,
					      src_pre, dst_pre, NULL });
	kfree(bufs);
	return err;
}

int yolo_journal_snapshot(struct yolo_sb_info *sbi, const char *name)
{
	/* A marker's gen is its position in the journal's P/T sequence, so no
	 * gen field is written — userspace derives it on parse. */
	return journal_write(sbi, 'P',
			     (const char *[]){ name, NULL });
}

/**
 * yolo_journal_travel - Append a travel record to the journal.
 * @sbi: superblock info (has journal_file)
 * @target_gen: the snapshot gen being traveled to
 *
 * Format: T\0<target_gen>\n  (the new gen is this record's own position,
 * derived by userspace on parse, so it is not written.)
 */
int yolo_journal_travel(struct yolo_sb_info *sbi, u16 target_gen)
{
	char target_str[6];

	snprintf(target_str, sizeof(target_str), "%u",
		 (unsigned)target_gen);
	return journal_write(sbi, 'T',
			     (const char *[]){ target_str, NULL });
}

static char gate_result_char(enum yolo_gate_result result)
{
	switch (result) {
	case YOLO_GATE_DIRECT_DENY:
		return 'd';
	case YOLO_GATE_ASK_ALLOW:
		return 'y';
	case YOLO_GATE_ASK_DENY:
		return 'n';
	default:
		return '\0';
	}
}

static char policy_code(enum yolo_perm perm)
{
	switch (perm) {
	case YOLO_PERM_UNSET:
		return 'u';
	case YOLO_PERM_ASK:
		return 'q';
	case YOLO_PERM_ALLOW:
		return 'a';
	case YOLO_PERM_WRITE_ASK:
		return 'w';
	case YOLO_PERM_READ_ONLY:
		return 'r';
	case YOLO_PERM_DENY:
		return 'd';
	default:
		return '\0';
	}
}

/* Resolve @dentry's overlay path into a freshly kmalloc'd PATH_MAX buffer.
 * On success returns the path pointer (inside *bufp, which the caller frees);
 * on failure returns an ERR_PTR and leaves nothing to free. A full PATH_MAX is
 * used — journal notes cover arbitrary-depth paths that YOLO_PATH_MAX truncates.
 */
static char *journal_dentry_path(struct dentry *dentry, char **bufp)
{
	char *buf = kmalloc(PATH_MAX, GFP_KERNEL);
	char *path;

	if (!buf)
		return ERR_PTR(-ENOMEM);
	path = dentry_path_raw(dentry, buf, PATH_MAX);
	if (IS_ERR(path)) {
		kfree(buf);
		return path;
	}
	*bufp = buf;
	return path;
}

/**
 * yolo_journal_gate - Append one prompted-or-denied access result.
 * @sbi: superblock info
 * @target: the dentry whose access reached the gate
 * @op: the attempted operation (enum yolo_op)
 * @result: d (static deny), y (asked/allow), or n (asked/deny)
 *
 * Observational note (does not set sbi->staging.dirty). Format:
 *   G\0<path>\0<op>\0<result>\n
 */
int yolo_journal_gate(struct yolo_sb_info *sbi, struct dentry *target,
		      enum yolo_op op, enum yolo_gate_result result)
{
	char op_str[2] = { op == YOLO_OP_WRITE ? 'w' : 'r', '\0' };
	char result_str[2] = { gate_result_char(result), '\0' };
	char *buf;
	char *path;
	int err;

	if (!result_str[0])
		return -EINVAL;
	path = journal_dentry_path(target, &buf);
	if (IS_ERR(path))
		return PTR_ERR(path);
	err = journal_write(sbi, 'G',
			    (const char *[]){ path, op_str, result_str, NULL });
	kfree(buf);
	return err;
}

/**
 * yolo_journal_configure - Append a successful live explicit-policy assignment.
 * @sbi: superblock info
 * @target: dentry on which the explicit policy is assigned
 * @perm: assigned policy, including UNSET
 *
 * Observational note (does not set sbi->staging.dirty). Format:
 *   C\0<path>\0<policy>\n
 */
int yolo_journal_configure(struct yolo_sb_info *sbi, struct dentry *target,
			   enum yolo_perm perm)
{
	char policy[2] = { policy_code(perm), '\0' };
	char *buf;
	char *path;
	int err;

	if (!policy[0])
		return -EINVAL;

	path = journal_dentry_path(target, &buf);
	if (IS_ERR(path))
		return PTR_ERR(path);
	err = journal_write(sbi, 'C',
			    (const char *[]){ path, policy, NULL });
	kfree(buf);
	return err;
}
