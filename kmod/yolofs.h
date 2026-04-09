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

/* ── Constants ─────────────────────────────────────────────────────── */

#define YOLO_SUPER_MAGIC	0xA6F5
#define YOLO_PATH_MAX		256

/* Jump tree buffer limits */
#define YOLO_JUMP_MAX_DEPTH		32
#define YOLO_JUMP_MAX_TREE_LEN		(16 * 1024 * 1024)

/* Operations passed in ask requests */
enum yolo_op {
	YOLO_OP_READ		= 1,
	YOLO_OP_WRITE		= 2,
	YOLO_OP_EXEC		= 3,
};

/* ── Permission Enum ───────────────────────────────────────────────── */

enum yolo_perm {
	YOLO_PERM_NONE		= 0,	/* no rule on this dentry */
	YOLO_PERM_ASK		= 1,	/* block thread, ask userspace */
	YOLO_PERM_ALLOW		= 2,	/* read + write + execute */
	YOLO_PERM_RO		= 3,	/* read + execute */
	YOLO_PERM_DENY		= 4,	/* all access denied */
	YOLO_PERM_HIDDEN	= 5,	/* path invisible: ENOENT on lookup/stat/open */
};

/* ── Ioctl Structures ──────────────────────────────────────────────── */

/*
 * All path fields use pointer + length instead of fixed-size arrays.
 * Paths are limited to YOLO_PATH_MAX-1 bytes (matching internal buffers).
 * The kernel copies path data via secondary copy_from_user / copy_to_user.
 */

struct yolo_ioc_rule {
	__u64	path_ptr;		/* userspace pointer to path string */
	__u16	path_len;		/* length excluding NUL */
	__u8	perm;			/* enum yolo_perm value */
	__u8	_pad[5];
};

/* Mark flags */
#define YOLO_MARK_IF_CHANGED	(1 << 0)	/* skip if no data records since last M/J */

/* userspace ↔ kernel: YOLO_IOC_MARK (name in, gen out) */
struct yolo_ioc_mark {
	__u64	gen;			/* out: assigned gen (0 if skipped) */
	__u64	name_ptr;		/* in: userspace pointer to name string */
	__u16	name_len;		/* in: length excluding NUL */
	__u8	flags;			/* in: YOLO_MARK_IF_CHANGED, etc. */
	__u8	_pad[5];
};

/* userspace ↔ kernel: YOLO_IOC_JUMP */
struct yolo_ioc_jump {
	__u64	target_gen;		/* in: mark gen to jump to (0 = reset) */
	__u64	new_gen;		/* out: new generation assigned (jump mode only) */
	__u64	tree_len;		/* in: byte length of serialized tree */
	__u64	tree_ptr;		/* in: userspace pointer to tree buffer */
};

#define YOLO_IOC_RULE_ADD	_IOW('A', 10, struct yolo_ioc_rule)
#define YOLO_IOC_RULE_REMOVE	_IOW('A', 11, struct yolo_ioc_rule)
#define YOLO_IOC_GET_REQUEST	_IOWR('A', 30, struct yolo_ctl_request)
#define YOLO_IOC_PUT_RESPONSE	_IOW('A', 31, struct yolo_ctl_response)
#define YOLO_IOC_MARK		_IOWR('A', 40, struct yolo_ioc_mark)
#define YOLO_IOC_JUMP		_IOWR('A', 41, struct yolo_ioc_jump)

/* ── Control-File Protocol (binary) ───────────────────────────────── */

/*
 * kernel → userspace: dequeued permission request.
 * Userspace provides path_ptr + path_buf_len; kernel writes path into
 * the buffer and sets path_len to the actual length.
 */
struct yolo_ctl_request {
	__u64	id;
	__u32	op;			/* YOLO_OP_READ / WRITE / EXEC */
	__u32	pid;
	char	comm[16];
	__u64	path_ptr;		/* in: userspace buffer for path */
	__u16	path_buf_len;		/* in: buffer capacity */
	__u16	path_len;		/* out: actual path length written */
	__u8	_pad[4];
};

/* userspace → kernel: write() accepts one of these */
struct yolo_ctl_response {
	__u64	id;
	__u8	decision;		/* enum yolo_perm value */
	__u8	_pad[7];
};

/* ── Internal: Pending Permission Request ──────────────────────────── */

struct yolo_perm_request {
	struct kref		ref;
	u64			id;
	char			path[YOLO_PATH_MAX];
	u16			path_len;
	enum yolo_op		op;
	pid_t			pid;
	char			comm[TASK_COMM_LEN];

	enum yolo_perm		decision;
	struct completion	done;
	struct list_head	list;
};

/* ── Dentry state ────────────────────────────────────────────── */

enum yolo_target {
	YOLO_TARGET_INODE	= 1,	/* staged inode in flat file store */
	YOLO_TARGET_PATH	= 2,	/* redirect to base path (also ground state) */
	YOLO_TARGET_NONE	= 3,	/* tombstone (pinned negative dentry) */
};

/* ── Ask Protocol Engine ───────────────────────────────────────────── */

struct yolo_ask_engine {
	struct list_head	pending_reqs;	/* requests waiting for daemon */
	spinlock_t		pending_lock;	/* protects pending_reqs */
	wait_queue_head_t	request_waitq;	/* daemon blocks here */
	atomic64_t		next_req_id;	/* unique request ID counter */
	unsigned int		timeout_s;	/* seconds before applying default */
	enum yolo_perm		default_perm;	/* decision when no daemon or timeout */

	/* Daemon connection (at most one — enforced by .ctl single-open) */
	atomic_t		has_daemon;	/* 1 if daemon connected, 0 otherwise */
	struct list_head	dispatched;	/* requests sent to daemon */
	spinlock_t		dispatch_lock;	/* protects dispatched */
};

/* ── Per-Superblock Info ───────────────────────────────────────────── */

struct yolo_sb_info {
	struct super_block	*lower_sb;
	struct path		base_path;	/* always "/" */
	struct path		storage_path;	/* ./yolofs/ directory */

	/* Control file */
	struct inode		*ctl_inode;	/* synthetic .ctl inode */
	struct dentry		*ctl_dentry;	/* pinned .ctl dentry */

	/* Staging */
	struct path		inodes_dir;	/* ./yolofs/inodes/ (sharded inode store) */
	struct file		*journal_file;	/* ./yolofs/journal (append-only, opened lazily) */
	struct rw_semaphore	staging_sem;	/* protects staging + journal writes */
	atomic_t		next_ino;	/* counter for inode store IDs */

	/* Inode store shard cache (avoids repeated lookups) */
	struct dentry		*shard_dentry;	/* cached current shard dir dentry */
	u32			shard_id;	/* which shard shard_dentry belongs to */
	atomic_t		gen;		/* bumped on each checkpoint; triggers re-COW */
	atomic_t		staging_fd_count;/* open staging write fds */
	bool			dirty;		/* data records written since last M/J */

	/* Permission gating */
	bool			permission;	/* enable/disable toggle */
	atomic64_t		perm_gen;	/* cache invalidation counter */
	struct yolo_ask_engine	ask_engine;	/* ask protocol state */
	struct list_head	pinned_rules;	/* dget()'d dentries with perm rules */
	spinlock_t		pinned_rules_lock;/* protects pinned_rules */

	bool			staging;
};

/* ── Per-Inode Info ────────────────────────────────────────────────── */

struct yolo_inode_info {
	struct inode		*lower_inode;
	enum yolo_perm		cached_perm;
	u64			perm_gen;
	u16			staging_gen;	/* generation when last staged/COW'd */

	struct inode		vfs_inode;	/* must be last for container_of */
};

/* ── Per-Dentry Info ───────────────────────────────────────────────── */

struct yolo_dentry_info {
	spinlock_t		lock;
	struct path		lower_path;	/* resolved lower path (inode entry or base) */
	enum yolo_target	target;		/* where content lives */
	bool			pinned;		/* held via dget by staging */
	enum yolo_perm		perm;		/* NONE unless explicit rule */
	struct list_head	rule_pin;	/* node in sbi->pinned_rules */
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
	       YOLO_I(d_inode(d))->staging_gen >= (u16)atomic_read(&sbi->gen);
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
		       u32 ino);
int yolo_journal_delete(struct yolo_sb_info *sbi, struct dentry *dentry);
int yolo_journal_rename(struct yolo_sb_info *sbi, struct dentry *old_dentry,
			  struct dentry *new_dentry);
int yolo_journal_mark(struct yolo_sb_info *sbi, u16 id, const char *name);
int yolo_journal_jump(struct yolo_sb_info *sbi, u16 gen, u16 target_gen);

/* perm.c */
static inline void yolo_perm_request_release(struct kref *kref)
{
	kfree(container_of(kref, struct yolo_perm_request, ref));
}
enum yolo_perm yolo_resolve_perm(struct dentry *dentry);
void yolo_cache_perm(struct inode *inode, struct dentry *dentry);
int yolo_check_perm(enum yolo_perm perm, int f_flags);
int yolo_check_dentry_perm(struct yolo_sb_info *sbi, struct dentry *dentry,
			   int f_flags, fmode_t f_mode);
int yolo_ask_userspace(struct yolo_sb_info *sbi, struct dentry *dentry,
		       const char *relpath, enum yolo_op op,
		       enum yolo_perm *result);

/* ioctl.c */
extern const struct file_operations yolo_ctl_fops;
void yolo_daemon_cleanup(struct yolo_sb_info *sbi);
void yolo_release_pinned_rules(struct yolo_sb_info *sbi);

#endif /* _YOLO_H_ */
