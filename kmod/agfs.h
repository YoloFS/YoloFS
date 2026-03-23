/* SPDX-License-Identifier: GPL-2.0 */
#ifndef _AGFS_H_
#define _AGFS_H_

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

#define AGFS_SUPER_MAGIC	0xA6F5
#define AGFS_PATH_MAX		256

/* Restore tree buffer limits */
#define AGFS_RESTORE_MAX_DEPTH		32
#define AGFS_RESTORE_MAX_TREE_LEN	(16 * 1024 * 1024)

/* Operations passed in ask requests */
enum agfs_op {
	AGFS_OP_READ		= 1,
	AGFS_OP_WRITE		= 2,
	AGFS_OP_EXEC		= 3,
};

/* ── Permission Enum ───────────────────────────────────────────────── */

enum agfs_perm {
	AGFS_PERM_NONE		= 0,	/* no rule on this dentry */
	AGFS_PERM_ASK		= 1,	/* block thread, ask userspace */
	AGFS_PERM_ALLOW		= 2,	/* read + write + execute */
	AGFS_PERM_ALLOW_RW	= 3,	/* read + write */
	AGFS_PERM_ALLOW_RO	= 4,	/* read only */
	AGFS_PERM_ALLOW_RX	= 5,	/* read + execute */
	AGFS_PERM_DENY		= 6,	/* all access denied */
};

/* ── Ioctl Structures ──────────────────────────────────────────────── */

/*
 * All path fields use pointer + length instead of fixed-size arrays.
 * Paths are limited to AGFS_PATH_MAX-1 bytes (matching internal buffers).
 * The kernel copies path data via secondary copy_from_user / copy_to_user.
 */

struct agfs_ioc_rule {
	__u64	path_ptr;		/* userspace pointer to path string */
	__u16	path_len;		/* length excluding NUL */
	__u8	perm;			/* enum agfs_perm value */
	__u8	_pad[5];
};

/* Checkpoint flags */
#define AGFS_CHK_IF_CHANGED	(1 << 0)	/* skip if no data records since last CKP/RST */

/* userspace ↔ kernel: AGFS_IOC_CHECKPOINT (name in, gen out) */
struct agfs_ioc_checkpoint {
	__u64	gen;			/* out: assigned gen (0 if skipped) */
	__u64	name_ptr;		/* in: userspace pointer to name string */
	__u16	name_len;		/* in: length excluding NUL */
	__u8	flags;			/* in: AGFS_CHK_IF_CHANGED, etc. */
	__u8	_pad[5];
};

/* userspace ↔ kernel: AGFS_IOC_RESTORE */
struct agfs_ioc_restore {
	__u64	target_gen;		/* in: checkpoint gen to restore to (0 = reset) */
	__u64	new_gen;		/* out: new generation assigned (restore mode only) */
	__u64	tree_len;		/* in: byte length of serialized tree */
	__u64	tree_ptr;		/* in: userspace pointer to tree buffer */
};

#define AGFS_IOC_RULE_ADD	_IOW('A', 10, struct agfs_ioc_rule)
#define AGFS_IOC_RULE_REMOVE	_IOW('A', 11, struct agfs_ioc_rule)
#define AGFS_IOC_GET_REQUEST	_IOWR('A', 30, struct agfs_ctl_request)
#define AGFS_IOC_PUT_RESPONSE	_IOW('A', 31, struct agfs_ctl_response)
#define AGFS_IOC_CHECKPOINT	_IOWR('A', 40, struct agfs_ioc_checkpoint)
#define AGFS_IOC_RESTORE	_IOWR('A', 41, struct agfs_ioc_restore)

/* ── Control-File Protocol (binary) ───────────────────────────────── */

/*
 * kernel → userspace: dequeued permission request.
 * Userspace provides path_ptr + path_buf_len; kernel writes path into
 * the buffer and sets path_len to the actual length.
 */
struct agfs_ctl_request {
	__u64	id;
	__u32	op;			/* AGFS_OP_READ / WRITE / EXEC */
	__u32	pid;
	char	comm[16];
	__u64	path_ptr;		/* in: userspace buffer for path */
	__u16	path_buf_len;		/* in: buffer capacity */
	__u16	path_len;		/* out: actual path length written */
	__u8	_pad[4];
};

/* userspace → kernel: write() accepts one of these */
struct agfs_ctl_response {
	__u64	id;
	__u8	decision;		/* enum agfs_perm value */
	__u8	_pad[7];
};

/* ── Internal: Pending Permission Request ──────────────────────────── */

struct agfs_perm_request {
	struct kref		ref;
	u64			id;
	char			path[AGFS_PATH_MAX];
	u16			path_len;
	enum agfs_op		op;
	pid_t			pid;
	char			comm[TASK_COMM_LEN];

	enum agfs_perm		decision;
	struct completion	done;
	struct list_head	list;
};

/* ── Dentry state ────────────────────────────────────────────── */

enum agfs_dkind {
	AGFS_DKIND_PASSTHROUGH	= 0,
	AGFS_DKIND_STAGED_INODE	= 1,
	AGFS_DKIND_REDIRECT	= 2,
	AGFS_DKIND_TOMBSTONE	= 3,
};

/* ── Ask Protocol Engine ───────────────────────────────────────────── */

struct agfs_ask_engine {
	struct list_head	pending_reqs;	/* requests waiting for daemon */
	spinlock_t		pending_lock;	/* protects pending_reqs */
	wait_queue_head_t	request_waitq;	/* daemon blocks here */
	atomic64_t		next_req_id;	/* unique request ID counter */
	unsigned int		timeout_s;	/* seconds before applying default */
	enum agfs_perm		default_perm;	/* decision when no daemon or timeout */

	/* Daemon connection (at most one) */
	struct file		*daemon_file;	/* which fd is the daemon; NULL if none */
	struct list_head	dispatched;	/* requests sent to daemon */
	spinlock_t		dispatch_lock;	/* protects dispatched + daemon_file */
};

/* ── Per-Superblock Info ───────────────────────────────────────────── */

struct agfs_sb_info {
	struct super_block	*lower_sb;
	struct path		base_path;	/* always "/" */
	struct path		storage_path;	/* ./agfs/ directory */

	/* Staging */
	struct path		inodes_dir;	/* ./agfs/inodes/ (flat inode store) */
	struct file		*journal_file;	/* ./agfs/journal (append-only, opened lazily) */
	struct rw_semaphore	staging_sem;	/* protects staging + journal writes */
	atomic_t		next_ino;	/* counter for inode store IDs */
	atomic_t		gen;		/* bumped on each checkpoint; triggers re-COW */
	atomic_t		staging_fd_count;/* open staging write fds */
	bool			dirty;		/* data records written since last CKP/RST */

	/* Permission gating */
	bool			permission;	/* enable/disable toggle */
	atomic64_t		perm_gen;	/* cache invalidation counter */
	struct agfs_ask_engine	ask_engine;	/* ask protocol state */
	struct list_head	pinned_rules;	/* dget()'d dentries with perm rules */
	spinlock_t		pinned_rules_lock;/* protects pinned_rules */

	bool			staging;
};

/* ── Per-Inode Info ────────────────────────────────────────────────── */

struct agfs_inode_info {
	struct inode		*lower_inode;
	enum agfs_perm		cached_perm;
	u64			perm_gen;
	u16			staging_gen;	/* generation when last staged/COW'd */

	struct inode		vfs_inode;	/* must be last for container_of */
};

/* ── Per-Dentry Info ───────────────────────────────────────────────── */

struct agfs_dentry_info {
	spinlock_t		lock;
	struct path		lower_path;	/* resolved lower path (inode entry or base) */
	enum agfs_dkind		kind;		/* overlay state tag */
	bool			in_base;	/* path has content in base filesystem */
	enum agfs_perm		perm;		/* NONE unless explicit rule */
	struct list_head	rule_pin;	/* node in sbi->pinned_rules */
	struct dentry		*rule_dentry;	/* back-pointer for dput on release */
};

/* ── Per-File Info ─────────────────────────────────────────────────── */

struct agfs_file_info {
	struct file		*lower_file;
};

struct agfs_dir_info {
	struct agfs_file_info	fi;		/* must be first */
	loff_t			base_pos;	/* saved lower f_pos for readdir resume */
	loff_t			dirent_off;	/* virtual offset at end of phase 1 */
};

/* ── Accessor Macros ───────────────────────────────────────────────── */

static inline struct agfs_sb_info *AGFS_SB(const struct super_block *sb)
{
	return sb->s_fs_info;
}

static inline struct agfs_inode_info *AGFS_I(const struct inode *inode)
{
	return container_of(inode, struct agfs_inode_info, vfs_inode);
}

static inline struct agfs_dentry_info *AGFS_D(const struct dentry *dentry)
{
	return dentry->d_fsdata;
}

/* ── Dentry-centric queries ─────────────────────────────────────── */

/* True if dentry is a current-generation staged inode (no COW needed). */
static inline bool agfs_dentry_is_current(const struct dentry *d,
					   struct agfs_sb_info *sbi)
{
	return AGFS_D(d)->kind == AGFS_DKIND_STAGED_INODE &&
	       AGFS_I(d_inode(d))->staging_gen >= (u16)atomic_read(&sbi->gen);
}

static inline struct agfs_file_info *AGFS_F(const struct file *file)
{
	return file->private_data;
}

static inline struct agfs_dir_info *AGFS_DI(const struct file *file)
{
	return file->private_data;
}

/* ── Lower-Path Helpers ────────────────────────────────────────────── */

static inline void agfs_get_lower_path(const struct dentry *dentry,
					struct path *lower_path)
{
	struct agfs_dentry_info *info = AGFS_D(dentry);

	spin_lock(&info->lock);
	path_get(&info->lower_path);
	*lower_path = info->lower_path;
	spin_unlock(&info->lock);
}

static inline void agfs_put_lower_path(const struct dentry *dentry,
					struct path *lower_path)
{
	path_put(lower_path);
}

static inline void agfs_set_lower_path(const struct dentry *dentry,
					struct path *lower_path)
{
	struct agfs_dentry_info *info = AGFS_D(dentry);

	spin_lock(&info->lock);
	info->lower_path = *lower_path;
	spin_unlock(&info->lock);
}

static inline void agfs_replace_lower_path(const struct dentry *dentry,
					    struct path *lower_path)
{
	struct agfs_dentry_info *info = AGFS_D(dentry);
	struct path old;

	spin_lock(&info->lock);
	old = info->lower_path;
	info->lower_path = *lower_path;
	spin_unlock(&info->lock);
	if (old.dentry)
		path_put(&old);
}

static inline void agfs_put_reset_lower_path(const struct dentry *dentry)
{
	struct agfs_dentry_info *info = AGFS_D(dentry);
	struct path lower_path;

	spin_lock(&info->lock);
	lower_path = info->lower_path;
	info->lower_path.dentry = NULL;
	info->lower_path.mnt = NULL;
	spin_unlock(&info->lock);
	path_put(&lower_path);
}

static inline struct inode *agfs_lower_inode(const struct inode *i)
{
	return AGFS_I(i)->lower_inode;
}

static inline void agfs_set_lower_inode(struct inode *i, struct inode *lower)
{
	AGFS_I(i)->lower_inode = lower;
}

static inline struct dentry *agfs_lower_dentry(const struct dentry *d)
{
	struct agfs_dentry_info *info = AGFS_D(d);
	return info ? info->lower_path.dentry : NULL;
}

static inline struct vfsmount *agfs_lower_mnt(const struct dentry *d)
{
	struct agfs_dentry_info *info = AGFS_D(d);
	return info ? info->lower_path.mnt : NULL;
}

/* ── Extern Declarations ───────────────────────────────────────────── */

/* super.c */
extern const struct super_operations agfs_sops;
extern struct kmem_cache *agfs_inode_cachep;

/* inode.c */
extern const struct inode_operations agfs_dir_iops;
extern const struct inode_operations agfs_main_iops;
extern const struct inode_operations agfs_symlink_iops;

/* file.c */
extern const struct file_operations agfs_main_fops;
extern const struct address_space_operations agfs_aops;

/* dir.c */
extern const struct file_operations agfs_dir_fops;

/* dentry.c */
extern const struct dentry_operations agfs_dops;
int agfs_init_dentry_cache(void);
void agfs_destroy_dentry_cache(void);
void agfs_dentry_stage(struct dentry *dentry, enum agfs_dkind kind,
		       bool in_base);
void agfs_dentry_stage_inode(struct dentry *dentry, struct agfs_sb_info *sbi,
			     bool in_base);
void agfs_dentry_unstage(struct dentry *dentry);
struct dentry *agfs_dentry_add_tombstone(struct dentry *parent,
					 const char *name, unsigned int len);
void agfs_dentry_remove_tombstone(struct dentry *tomb);
int agfs_dentry_inject(struct dentry *parent, const u8 *name,
		       u16 name_len, struct super_block *sb,
		       struct path *lower_path,
		       enum agfs_dkind kind, bool in_base, u16 gen);
void agfs_unstage_all(struct super_block *sb);

/* lookup.c */
struct dentry *agfs_lookup(struct inode *dir, struct dentry *dentry,
			   unsigned int flags);
struct inode *agfs_iget(struct super_block *sb, struct inode *lower_inode);
int agfs_interpose(struct dentry *dentry, struct super_block *sb,
		   struct path *lower_path);

/* staging.c */
int agfs_inode_path(struct agfs_sb_info *sbi, u32 ino,
		    struct path *result);
int agfs_inode_alloc(struct agfs_sb_info *sbi, u32 *out_ino,
		     struct path *inode_path, umode_t mode,
		     const char *symname);
int agfs_do_cow(struct agfs_sb_info *sbi, struct dentry *dentry,
		struct file **new_file, int flags, bool truncate);

/* journal.c */
int agfs_journal_open(struct agfs_sb_info *sbi);
int agfs_journal_add(struct agfs_sb_info *sbi, struct dentry *dentry,
		       u32 ino, unsigned char d_type);
int agfs_journal_modify(struct agfs_sb_info *sbi, struct dentry *dentry,
			  u32 ino, unsigned char d_type);
int agfs_journal_delete(struct agfs_sb_info *sbi, struct dentry *dentry,
			 unsigned char d_type);
int agfs_journal_rename(struct agfs_sb_info *sbi, struct dentry *old_dentry,
			  struct dentry *new_dentry, unsigned char d_type);
int agfs_journal_replace(struct agfs_sb_info *sbi, struct dentry *old_dentry,
			   struct dentry *new_dentry, unsigned char d_type);
int agfs_journal_checkpoint(struct agfs_sb_info *sbi, u16 id, const char *name);
int agfs_journal_restore(struct agfs_sb_info *sbi, u16 gen, u16 target_gen);

/* perm.c */
static inline void agfs_perm_request_release(struct kref *kref)
{
	kfree(container_of(kref, struct agfs_perm_request, ref));
}
enum agfs_perm agfs_resolve_perm(struct dentry *dentry);
void agfs_cache_perm(struct inode *inode, struct dentry *dentry);
int agfs_check_perm(enum agfs_perm perm, int f_flags);
int agfs_ask_userspace(struct agfs_sb_info *sbi, struct dentry *dentry,
		       const char *relpath, enum agfs_op op,
		       enum agfs_perm *result);

/* ioctl.c */
long agfs_ioctl(struct file *file, unsigned int cmd, unsigned long arg);
void agfs_daemon_cleanup(struct agfs_sb_info *sbi);
void agfs_release_pinned_rules(struct agfs_sb_info *sbi);

#endif /* _AGFS_H_ */
