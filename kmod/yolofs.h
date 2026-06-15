/* SPDX-License-Identifier: GPL-2.0 */
#ifndef _YOLO_H_
#define _YOLO_H_

#include <linux/fs.h>
#include <linux/fs_stack.h>
#include <linux/cred.h>
#include <linux/namei.h>
#include <linux/dcache.h>
#include <linux/mount.h>
#include <linux/path.h>
#include <linux/slab.h>
#include <linux/spinlock.h>
#include <linux/rwsem.h>
#include <linux/atomic.h>
#include <linux/wait.h>
#include <linux/completion.h>
#include <linux/list.h>
#include <linux/stringhash.h>
#include <linux/poll.h>
#include <linux/ioctl.h>
#include <linux/kref.h>
#include <linux/ktime.h>
#include <linux/uaccess.h>
#include <linux/magic.h>
#include <linux/module.h>

/* ── User/Kernel ABI ───────────────────────────────────────────────────
 *
 * Everything below this banner (up to the next) is the binary contract with
 * userspace and must stay in sync with the mirrored definitions in
 * user/ioctl.rs — its struct_sizes test pins the layout. yolofs has no
 * backward-compat requirement, so these may change freely; just update both
 * sides together.
 */

#define YOLO_PATH_MAX		256

/* Operations passed in ask requests */
enum yolo_op {
	YOLO_OP_READ		= 1,
	YOLO_OP_WRITE		= 2,
};

/* Permission rule modes */
enum yolo_perm {
	YOLO_PERM_UNSET		= 0,	/* no rule on this dentry */
	YOLO_PERM_ASK		= 1,	/* block thread, ask userspace */
	YOLO_PERM_ALLOW		= 2,	/* read + write + execute */
	YOLO_PERM_WRITE_ASK	= 3,	/* read + execute; writes ask userspace */
	YOLO_PERM_READ_ONLY	= 4,	/* read + execute; writes denied */
	YOLO_PERM_DENY		= 5,	/* all access denied */
	YOLO_PERM_HIDE		= 6,	/* path invisible: ENOENT on lookup/stat/open */
};

/* One-shot ask decisions */
enum yolo_decision {
	YOLO_DECISION_DENY	= 0,	/* fail the current operation */
	YOLO_DECISION_ALLOW	= 1,	/* allow the current operation */
};

/* userspace ↔ kernel: YOLO_IOC_RULE_SET / RULE_RESOLVE. The target is an
 * O_PATH fd opened through the mount — resolved once, by the caller's own
 * open(); the kernel never re-walks a path string, checks the exact dentry
 * the rule attaches to, and rule paths are not capped by YOLO_PATH_MAX. */
struct yolo_ioc_rule {
	__s32	fd;			/* in: O_PATH fd of the rule target */
	__u8	perm;			/* in: enum yolo_perm (UNSET clears); out for RESOLVE */
	__u8	_pad[3];
};

/* Snapshot flags */
#define YOLO_SNAPSHOT_IF_CHANGED	(1 << 0)	/* skip if no data records since last P/T */

/* userspace ↔ kernel: YOLO_IOC_SNAPSHOT (name in, gen out) */
struct yolo_ioc_snapshot {
	__u64	gen;			/* out: assigned gen (0 if skipped) */
	__u64	name_ptr;		/* in: userspace pointer to name string */
	__u16	name_len;		/* in: length excluding NUL */
	__u8	flags;			/* in: YOLO_SNAPSHOT_IF_CHANGED, etc. */
	__u8	_pad[5];
};

/* userspace ↔ kernel: YOLO_IOC_TRAVEL (gen 0 = base; gen >= 1 = snapshot/travel) */
struct yolo_ioc_travel {
	__u64	target_gen;		/* in: generation to travel to */
	__u64	new_gen;		/* out: new generation assigned */
	__u64	tree_len;		/* in: byte length of serialized tree */
	__u64	tree_ptr;		/* in: userspace pointer to tree buffer */
};

/* userspace ↔ kernel: replace the in-memory staged view without journaling.
 * The two floors are both "max S-record ino" thresholds, differing in scope:
 * alloc_ino_floor spans the full journal (dead segments and deleted files
 * still occupy the store, so allocation must resume above every ino ever
 * issued); cow_ino_floor stops at the latest P/T marker (injected inodes
 * above it are live-segment edits and stamp at gen, write-in-place; at or
 * below it they are snapshot-retained and stamp at gen-1, re-COW on first
 * write). They coincide exactly when the live segment has no S records. */
struct yolo_ioc_restore {
	__u64	gen;			/* in: latest journal marker generation */
	__u64	tree_len;		/* in: byte length of serialized tree */
	__u64	tree_ptr;		/* in: userspace pointer to tree buffer */
	__u32	alloc_ino_floor;	/* in: allocate store inos above this */
	__u32	cow_ino_floor;		/* in: write-in-place only above this */
	__u8	dirty;			/* in: live S/D/R exists after latest P/T */
	__u8	_pad[7];
};

/* kernel → userspace: dequeued permission ask */
struct yolo_ioc_ask {
	__u64	id;
	__u32	op;			/* YOLO_OP_READ / WRITE */
	__u32	pid;
	char	comm[16];
	__u16	access_path_len;	/* out: bytes in access_path */
	__u16	rule_path_len;		/* out: bytes in rule_path */
	__u8	rule_perm;		/* enum yolo_perm value */
	__u8	_pad[3];
	char	access_path[YOLO_PATH_MAX];
	char	rule_path[YOLO_PATH_MAX];	/* empty means default ask */
};

/* userspace → kernel: answer a pending ask */
struct yolo_ioc_decision {
	__u64	id;
	__u8	decision;		/* enum yolo_decision value */
	__u8	_pad[7];
};

#define YOLO_IOC_RULE_SET	_IOW('A', 10, struct yolo_ioc_rule)
#define YOLO_IOC_RULE_RESOLVE	_IOWR('A', 11, struct yolo_ioc_rule)
#define YOLO_IOC_GET_ASK	_IOR('A', 30, struct yolo_ioc_ask)
#define YOLO_IOC_PUT_DECISION	_IOW('A', 31, struct yolo_ioc_decision)
#define YOLO_IOC_SNAPSHOT	_IOWR('A', 40, struct yolo_ioc_snapshot)
#define YOLO_IOC_TRAVEL		_IOWR('A', 41, struct yolo_ioc_travel)
#define YOLO_IOC_RESTORE	_IOW('A', 42, struct yolo_ioc_restore)

/* ── Constants ─────────────────────────────────────────────────────── */

#define YOLO_SUPER_MAGIC	0xA6F5

/* Travel tree buffer limits */
#define YOLO_TRAVEL_MAX_DEPTH		32
#define YOLO_TRAVEL_MAX_TREE_LEN		(16 * 1024 * 1024)

/* ── Internal: Pending Permission Request ──────────────────────────────
 *
 * Ownership & locking (perm->pending_lock guards both request lists and the
 * `dispatched` flag):
 *   - The requesting thread holds one ref for the req's whole life: kref_init
 *     in yolo_ask_userspace() down to its final kref_put().
 *   - A req lives on exactly one of perm->pending_reqs (awaiting dequeue) or
 *     perm->dispatched (handed to the daemon), or on neither once resolved;
 *     `dispatched` records which.
 *   - While on the dispatched list it carries a second ref (taken in GET_ASK),
 *     dropped by whoever unlinks it there — PUT_DECISION, daemon cleanup, or a
 *     failed GET_ASK delivery. So a timed-out / interrupted requester leaves a
 *     dispatched req in place rather than unlinking it itself.
 */

struct yolo_ask {
	struct kref		ref;
	u64			id;
	char			access_path[YOLO_PATH_MAX];
	u16			access_path_len;
	char			rule_path[YOLO_PATH_MAX];
	u16			rule_path_len;
	enum yolo_perm		rule_perm;
	enum yolo_op		op;
	pid_t			pid;
	char			comm[TASK_COMM_LEN];

	enum yolo_decision	decision;
	bool			decided;
	struct completion	done;
	struct list_head	list;
	bool			dispatched;	/* true while on perm->dispatched */
};

/* ── Dentry state ────────────────────────────────────────────── */

enum yolo_target {
	YOLO_TARGET_INODE	= 1,	/* staged inode in flat file store */
	YOLO_TARGET_PATH	= 2,	/* redirect to base path (also ground state) */
	YOLO_TARGET_NONE	= 3,	/* tombstone (pinned negative dentry) */
};

/* ── Staging ───────────────────────────────────────────────────────── */

struct yolo_staging {
	bool			enabled;	/* staging on/off toggle */
	struct path		inodes_dir;	/* ./yolofs/inodes/ (sharded inode store) */
	struct file		*journal_file;	/* ./yolofs/journal (append-only, opened lazily) */
	struct rw_semaphore	sem;		/* protects staging + journal writes */
	atomic_t		next_ino;	/* counter for inode store IDs */

	/* Inode store shard cache (avoids repeated lookups). shard_lock guards
	 * shard_dentry + shard_id + shard_epoch: creates reach the cache without
	 * staging.sem (they only hold the parent dir's i_rwsem, which doesn't
	 * serialize creates in different directories). shard_epoch is bumped by
	 * quiesce so a create that raced past it cannot re-publish a stale
	 * shard dentry into the cache afterwards. */
	spinlock_t		shard_lock;
	struct dentry		*shard_dentry;	/* cached current shard dir dentry */
	u32			shard_id;	/* which shard shard_dentry belongs to */
	u64			shard_epoch;	/* invalidation count, under shard_lock */
	atomic_t		gen;		/* bumped on each snapshot; triggers re-COW */
	atomic_t		fd_count;	/* open staging write fds */
	bool			dirty;		/* data records written since last P/T */
};

/* ── Permission gating: rules + the ask protocol ───────────────────── */

struct yolo_permission {
	bool			enabled;	/* permission gating on/off toggle */
	atomic64_t		gen;		/* cache invalidation counter */
	struct list_head	pinned_rules;	/* dget()'d dentries with perm rules */
	spinlock_t		pinned_rules_lock;/* protects pinned_rules */

	/* Ask protocol. pending_lock guards both request lists: pending_reqs
	 * (waiting to be dequeued) and dispatched (handed to the daemon,
	 * awaiting a decision). */
	struct list_head	pending_reqs;	/* requests waiting for daemon */
	struct list_head	dispatched;	/* requests sent to daemon */
	spinlock_t		pending_lock;	/* protects pending_reqs + dispatched */
	wait_queue_head_t	request_waitq;	/* daemon blocks here */
	atomic64_t		next_req_id;	/* unique request ID counter */
	unsigned int		timeout_s;	/* seconds to wait before denying */

	/* Daemon connection (at most one). The control ioctls live on the mount
	 * root directory, whose fd already uses private_data for the readdir
	 * cursor — so the daemon is tracked by file identity, not private_data.
	 * A non-NULL daemon_file is itself the "daemon connected" flag. */
	struct file		*daemon_file;	/* the fd that claimed the daemon */
};

/* ── Per-Superblock Info ───────────────────────────────────────────── */

struct yolo_sb_info {
	struct super_block	*lower_sb;	/* lower fs superblock (kept via s_active) */
	struct path		storage_path;	/* ./yolofs/ directory */

	struct yolo_staging	staging;	/* staging area + inode store */
	struct yolo_permission	perm;		/* gating rules + ask protocol */
};

/* ── Per-Inode Info ────────────────────────────────────────────────── */

struct yolo_inode_info {
	struct inode		*lower_inode;
	enum yolo_perm		cached_perm;
	u64			perm_gen;
	u16			staging_gen;	/* generation when last staged/COW'd */
	u32			staging_ino;	/* store id when target == INODE (0 = none) */

	struct inode		vfs_inode;	/* must be last for container_of */
};

/* ── Per-Dentry Info ───────────────────────────────────────────────── */

struct yolo_dentry_info {
	spinlock_t		lock;
	struct path		lower_path;	/* resolved lower path (inode entry or base) */
	enum yolo_target	target;		/* where content lives */
	bool			pinned;		/* held via dget by staging */
	enum yolo_perm		perm;		/* explicit rule or UNSET */
	struct list_head	rule_pin;	/* node in sbi->perm.pinned_rules */
	struct dentry		*rule_dentry;	/* back-pointer for dput on release */
};

/* ── Per-File Info ─────────────────────────────────────────────────── */

struct yolo_file_info {
	struct file		*lower_file;
};

struct yolo_dir_info {
	struct yolo_file_info	fi;		/* must be first */
	loff_t			base_pos;	/* saved lower f_pos for readdir resume */
	loff_t			dirent_off;	/* virtual offset at end of phase 1 */
	struct dentry		*phase1_cursor;	/* in-list phase-1 cursor */
};

/* ── Accessor Macros ───────────────────────────────────────────────── */

static inline struct yolo_sb_info *YOLO_SB(const struct super_block *sb)
{
	return sb->s_fs_info;
}

static inline struct yolo_inode_info *YOLO_I(const struct inode *inode)
{
	return container_of(inode, struct yolo_inode_info, vfs_inode);
}

static inline struct yolo_dentry_info *YOLO_D(const struct dentry *dentry)
{
	return dentry->d_fsdata;
}

/* ── Dentry-centric queries ─────────────────────────────────────── */

/* True if dentry is a current-generation staged inode (no COW needed). */
static inline bool yolo_dentry_is_current(const struct dentry *d,
					   struct yolo_sb_info *sbi)
{
	return YOLO_D(d)->target == YOLO_TARGET_INODE &&
	       YOLO_I(d_inode(d))->staging_gen >= (u16)atomic_read(&sbi->staging.gen);
}

static inline struct yolo_file_info *YOLO_F(const struct file *file)
{
	return file->private_data;
}

static inline struct yolo_dir_info *YOLO_DI(const struct file *file)
{
	return file->private_data;
}

/* ── Lower-Path Helpers ────────────────────────────────────────────── */

static inline void yolo_get_lower_path(const struct dentry *dentry,
					struct path *lower_path)
{
	struct yolo_dentry_info *info = YOLO_D(dentry);

	spin_lock(&info->lock);
	path_get(&info->lower_path);
	*lower_path = info->lower_path;
	spin_unlock(&info->lock);
}

static inline void yolo_put_lower_path(const struct dentry *dentry,
					struct path *lower_path)
{
	path_put(lower_path);
}

/* Tagged operation-local pre-image target of @dentry, written into @buf. The
 * tag is the lowercased first letter of the userspace `Target` variant it
 * parses to, so the pre namespace never shares letters with the record tags:
 *   "a"            Absence: negative dentry / tombstone / unresolvable
 *   "s:<ino>"      StagedFile: staged inode (target == YOLO_TARGET_INODE)
 *   "b:<abspath>"  BasePath: redirect-resolved base content (PATH or ground)
 * This is the exact pre-op backing — an already-staged file reports its staged
 * inode (s:), not the base it was COW'd from. The CLI parses this into a
 * `Target` to seed a review range's old side; see docs/staging.md. */
static inline const char *yolo_preimage_target(const struct dentry *dentry,
					       char *buf, int len)
{
	struct yolo_dentry_info *di = YOLO_D(dentry);
	struct path lower;
	char *p;

	if (d_is_negative(dentry) || di->target == YOLO_TARGET_NONE)
		return "a";

	if (di->target == YOLO_TARGET_INODE) {
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

static inline void yolo_set_lower_path(const struct dentry *dentry,
					struct path *lower_path)
{
	struct yolo_dentry_info *info = YOLO_D(dentry);

	spin_lock(&info->lock);
	info->lower_path = *lower_path;
	spin_unlock(&info->lock);
}

static inline void yolo_replace_lower_path(const struct dentry *dentry,
					    struct path *lower_path)
{
	struct yolo_dentry_info *info = YOLO_D(dentry);
	struct path old;

	spin_lock(&info->lock);
	old = info->lower_path;
	info->lower_path = *lower_path;
	spin_unlock(&info->lock);
	if (old.dentry)
		path_put(&old);
}

static inline void yolo_put_reset_lower_path(const struct dentry *dentry)
{
	struct yolo_dentry_info *info = YOLO_D(dentry);
	struct path lower_path;

	spin_lock(&info->lock);
	lower_path = info->lower_path;
	info->lower_path.dentry = NULL;
	info->lower_path.mnt = NULL;
	spin_unlock(&info->lock);
	path_put(&lower_path);
}

static inline struct inode *yolo_lower_inode(const struct inode *i)
{
	return YOLO_I(i)->lower_inode;
}

static inline void yolo_set_lower_inode(struct inode *i, struct inode *lower)
{
	YOLO_I(i)->lower_inode = lower;
}

static inline struct dentry *yolo_lower_dentry(const struct dentry *d)
{
	struct yolo_dentry_info *info = YOLO_D(d);
	return info ? info->lower_path.dentry : NULL;
}

static inline struct vfsmount *yolo_lower_mnt(const struct dentry *d)
{
	struct yolo_dentry_info *info = YOLO_D(d);
	return info ? info->lower_path.mnt : NULL;
}

/* ── Extern Declarations ───────────────────────────────────────────── */

/* super.c */
extern const struct super_operations yolo_sops;
extern struct kmem_cache *yolo_inode_cachep;

/* inode.c */
extern const struct inode_operations yolo_dir_iops;
extern const struct inode_operations yolo_main_iops;
extern const struct inode_operations yolo_symlink_iops;

/* file.c */
extern const struct file_operations yolo_main_fops;
extern const struct address_space_operations yolo_aops;

/* dir.c */
extern const struct file_operations yolo_dir_fops;

/* dentry.c */
extern const struct dentry_operations yolo_dops;
int yolo_init_dentry_cache(void);
void yolo_destroy_dentry_cache(void);
int yolo_dentry_interpose(struct dentry *dentry, struct path *lower_path);
struct dentry *yolo_dentry_create(struct dentry *parent,
				  const char *name, unsigned int len,
				  enum yolo_target target,
				  struct path *lower_path);
void yolo_dentry_pin(struct dentry *dentry, enum yolo_target target);
void yolo_dentry_unpin(struct dentry *dentry);
void yolo_dentry_unpin_all(struct super_block *sb);

/* lookup.c */
struct dentry *yolo_lookup(struct inode *dir, struct dentry *dentry,
			   unsigned int flags);
struct inode *yolo_iget(struct super_block *sb, struct inode *lower_inode);

/* staging.c */
int yolo_inode_path(struct yolo_sb_info *sbi, u32 ino,
		    struct path *result);
int yolo_inode_alloc(struct yolo_sb_info *sbi, u32 *out_ino,
		     struct path *inode_path, umode_t mode,
		     const char *symname);
int yolo_do_cow(struct yolo_sb_info *sbi, struct dentry *dentry,
		struct file **new_file, int flags, bool truncate);

/* journal.c */
int yolo_journal_open(struct yolo_sb_info *sbi);
int yolo_journal_stage(struct yolo_sb_info *sbi, struct dentry *dentry,
		       u32 ino, const char *pre);
int yolo_journal_delete(struct yolo_sb_info *sbi, struct dentry *dentry);
int yolo_journal_rename(struct yolo_sb_info *sbi, struct dentry *old_dentry,
			  struct dentry *new_dentry);
int yolo_journal_snapshot(struct yolo_sb_info *sbi, const char *name);
int yolo_journal_travel(struct yolo_sb_info *sbi, u16 target_gen);
int yolo_journal_block(struct yolo_sb_info *sbi, struct dentry *target,
		       struct dentry *checked, enum yolo_op op);
int yolo_journal_ask(struct yolo_sb_info *sbi, const char *path,
		     enum yolo_op op, enum yolo_decision decision);

/* perm.c */
static inline void yolo_ask_release(struct kref *kref)
{
	kfree(container_of(kref, struct yolo_ask, ref));
}
enum yolo_op yolo_open_op(int f_flags);
enum yolo_perm yolo_perm_walk(struct dentry *dentry, struct dentry **source);
void yolo_perm_refresh(struct inode *inode, struct dentry *dentry);
enum yolo_perm yolo_perm_get(struct inode *inode, struct dentry *dentry);
int yolo_perm_check_dentry(struct yolo_sb_info *sbi, struct dentry *dentry,
			   int f_flags, bool *ask_resolved);
int yolo_ask_userspace(struct yolo_sb_info *sbi, const char *access_path,
		       const char *rule_path, enum yolo_perm rule_perm,
		       enum yolo_op op, enum yolo_decision *result);

/* ioctl.c — control ioctls live on the mount-root directory (yolo_dir_fops). */
long yolo_ctl_ioctl(struct file *file, unsigned int cmd, unsigned long arg);
void yolo_ctl_release(struct file *file);
void yolo_release_pinned_rules(struct yolo_sb_info *sbi);

#endif /* _YOLO_H_ */
