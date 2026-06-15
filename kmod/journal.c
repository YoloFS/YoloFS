// SPDX-License-Identifier: GPL-2.0
/*
 * yolofs — append-only journal.
 *
 * Written by the kernel on every mutation. Read by the CLI for
 * commit/abort/status/diff. The kernel never reads it back.
 *
 * Record format (NUL-separated fields, newline-terminated):
 *   S\0<path>\0<ino>\0<pre>\n          — Stage (post = StagedFile(ino))
 *   D\0<path>\0<pre>\n                 — Delete (post = Absent)
 *   R\0<dst>\0<src>\0<src_pre>\0<dst_pre>\n  — Rename
 *   P\0<name>\n                       — Snapshot
 *   T\0<target_gen>\n                 — Travel
 *   A\0<path>\0<op>\0<decision>\n      — Ask resolved (observational)
 *   B\0<path>\0<op>\0<rule_path>\n     — Blocked by a static rule (observational)
 *   (op = r/w; decision = y/n — the yes/no answer to the ask)
 *
 * A and B are disjoint, one per access: an ask resolved to deny is recorded
 * solely as A; B is only for static-rule blocks (deny, read-only-on-write)
 * that never prompted, and carries the blocking rule's path.
 *
 * Record tags are uppercase. Each *pre field is the operation-local pre-op
 * backing of that overlay name, tagged with the lowercased first letter of the
 * userspace Target variant: "a" (Absence), "s:<ino>" (StagedFile), "b:<abspath>"
 * (BasePath). See yolo_preimage_target() and docs/staging.md.
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

	sbi->staging.journal_file = f;
	return 0;
}

/* ── Write one record ───────────────────────────────────────────────── */

static int journal_write(struct yolo_sb_info *sbi, char tag,
			 const char **fields)
{
	const size_t bufsz = 4 * YOLO_PATH_MAX + 64;
	char *buf;
	size_t off = 0;
	const char **fp;
	struct file *f = sbi->staging.journal_file;
	loff_t pos;
	int err;

	if (!f)
		return -EIO;

	/* Off the stack: a worst-case R record holds four path-sized fields. */
	buf = kmalloc(bufsz, GFP_KERNEL);
	if (!buf)
		return -ENOMEM;

	buf[off++] = tag;
	buf[off++] = '\0';
	for (fp = fields; *fp; fp++) {
		size_t len = strlen(*fp);

		if (off + len + 1 > bufsz) {
			kfree(buf);
			return -ENAMETOOLONG;
		}
		memcpy(buf + off, *fp, len);
		off += len;
		buf[off++] = (*(fp + 1)) ? '\0' : '\n';
	}

	pos = f->f_pos;
	err = kernel_write(f, buf, off, &pos);
	kfree(buf);
	/* Only data mutations (S/D/R) mark the session dirty. Markers (P/T) and
	 * observational notes (A/B) are excluded — they must not trigger an
	 * auto-snapshot under YOLO_SNAPSHOT_IF_CHANGED. */
	if (err >= 0 && (tag == 'S' || tag == 'D' || tag == 'R'))
		WRITE_ONCE(sbi->staging.dirty, true);
	return err < 0 ? err : 0;
}

/* ── Helpers ───────────────────────────────────────────────────────── */

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

	/* `pre` is the tagged pre-op target (a / s:<ino> / b:<path>), captured by
	 * the caller before the lower_path swap. The post-target is StagedFile(ino). */
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
					       yolo_preimage_target(dentry, pre_buf,
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
	src_pre = yolo_preimage_target(old_dentry, src_pre_buf, YOLO_PATH_MAX);
	dst_pre = d_is_positive(new_dentry)
		? yolo_preimage_target(new_dentry, dst_pre_buf, YOLO_PATH_MAX)
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

/* Single-letter journal encodings (self-describing, like the record tags). */
static char op_char(enum yolo_op op)
{
	return op == YOLO_OP_WRITE ? 'w' : 'r';
}

static char decision_char(enum yolo_decision decision)
{
	return decision == YOLO_DECISION_ALLOW ? 'y' : 'n';
}

/**
 * yolo_journal_ask - Append an "ask resolved" note recording the decision.
 * @sbi: superblock info
 * @path: the asked path (relative, as sent to the daemon)
 * @op: the attempted operation (enum yolo_op)
 * @decision: allow/deny decision from the daemon or timeout default
 *
 * Observational note (does not set sbi->staging.dirty). Format:
 *   A\0<path>\0<op>\0<decision>\n   (op = r/w; decision = y/n)
 */
int yolo_journal_ask(struct yolo_sb_info *sbi, const char *path,
		     enum yolo_op op, enum yolo_decision decision)
{
	char op_str[2] = { op_char(op), '\0' };
	char dec_str[2] = { decision_char(decision), '\0' };

	return journal_write(sbi, 'A',
			     (const char *[]){ path, op_str, dec_str, NULL });
}

/**
 * yolo_journal_block - Append a "permission blocked" note to the journal.
 * @sbi: superblock info (has journal_file)
 * @target: the dentry whose access was denied — the agent's intended target
 *          (the file for opens/setattr; the child for parent-write-denied
 *          mutates). Recorded as the <path> field.
 * @checked: the dentry whose rule gated the access (the file itself, or the
 *           parent for mutates). Its nearest ruled ancestor is the blocking
 *           rule, recorded as the <rule_path> field.
 * @op: the attempted operation (enum yolo_op)
 *
 * Observational note: written only for a *static*-rule block (deny, or
 * read-only on a write) — an ask resolved to deny is recorded solely as an A
 * note (see yolo_perm_check_dentry's @ask_resolved), never here. Does not set
 * sbi->staging.dirty (see journal_write).
 *
 * Format: B\0<path>\0<op>\0<rule_path>\n
 */
int yolo_journal_block(struct yolo_sb_info *sbi, struct dentry *target,
		       struct dentry *checked, enum yolo_op op)
{
	char path_buf[YOLO_PATH_MAX];
	char rule_buf[YOLO_PATH_MAX];
	char op_str[2] = { op_char(op), '\0' };
	const char *rule_path = "";
	struct dentry *rule = NULL;
	char *path = dentry_path_raw(target, path_buf, sizeof(path_buf));
	if (IS_ERR(path))
		return PTR_ERR(path);

	/* Resolve the blocking rule from the *checked* dentry — its nearest
	 * ruled ancestor (the parent for mutates), not the target's. A static
	 * block always has a source; the NULL/format-error fallback to "" keeps
	 * this best-effort. */
	yolo_perm_walk(checked, &rule);
	if (rule) {
		const char *p = dentry_path_raw(rule, rule_buf, sizeof(rule_buf));
		if (!IS_ERR(p))
			rule_path = p;
		dput(rule);
	}

	return journal_write(sbi, 'B',
			     (const char *[]){ path, op_str, rule_path, NULL });
}
