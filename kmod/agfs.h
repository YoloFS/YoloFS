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

/* Dirent ino discrimination */
#define AGFS_INO_DELETED	0ULL
#define AGFS_INO_REDIRECT	((u64)-1)

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

/* userspace → kernel: one entry in AGFS_IOC_RESTORE */
struct agfs_ioc_restore_entry {
	__u64	path_ptr;		/* userspace pointer to path string */
	__u16	path_len;		/* length excluding NUL */
	__u8	d_type;			/* DT_REG / DT_DIR / DT_LNK */
	__u8	overwrites;		/* 1 if this path had existing content */
	__u8	_pad1[4];
	__u64	ino;			/* inode store ID; 0 = deleted */
	__u64	base_ptr;		/* userspace pointer to base path string */
	__u16	base_len;		/* length excluding NUL; 0 = none */
	__u8	_pad2[6];
};

/* userspace ↔ kernel: AGFS_IOC_RESTORE */
struct agfs_ioc_restore {
	__u64	target_gen;		/* in: checkpoint gen to restore to (0 = reset) */
	__u64	new_gen;		/* out: new generation assigned (restore mode only) */
	__u64	entry_count;
	__u64	entries_ptr;
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
	enum agfs_op		op;
	pid_t			pid;
	char			comm[TASK_COMM_LEN];

	enum agfs_perm		decision;
	struct completion	done;
	struct list_head	list;
};

/* ── Per-directory dirent ─────────────────────────────────────── */

struct agfs_dirent {
	struct hlist_node	node;
	u64			ino;		/* >0 = staged, 0 = deleted, (u64)-1 = redirect */
	char			*base;		/* redirect: source path; else NULL */
	u64			gen;		/* sbi->gen when inode was created */
	unsigned int		name_len;
	unsigned char		d_type;		/* DT_REG / DT_DIR / DT_LNK for readdir */
	bool			overwrites;	/* path had existing content */
	char			name[];
};


static inline bool agfs_ino_is_staged(u64 ino)
{
	return ino != AGFS_INO_DELETED && ino != AGFS_INO_REDIRECT;
}

static inline bool agfs_ino_is_redirect(u64 ino)
{
	return ino == AGFS_INO_REDIRECT;
}

static inline bool agfs_ino_is_deleted(u64 ino)
{
	return ino == AGFS_INO_DELETED;
}

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
	atomic64_t		next_ino;	/* counter for inode store IDs */
	atomic64_t		gen;		/* bumped on each checkpoint; triggers re-COW */
	atomic_t		staging_fd_count;/* open staging write fds */
	bool			dirty;		/* data records written since last CKP/RST */
	struct list_head	pinned_dirs;	/* igrab()'d directory inodes with dirents */
	spinlock_t		pinned_dirs_lock;/* protects pinned_dirs */

	/* Permission gating */
	bool			permission;	/* enable/disable toggle */
	atomic64_t		perm_gen;	/* cache invalidation counter */
	struct agfs_ask_engine	ask_engine;	/* ask protocol state */
	struct list_head	pinned_rules;	/* dget()'d dentries with perm rules */
	spinlock_t		pinned_rules_lock;/* protects pinned_rules */

	bool			staging;
};

/* ── Per-Inode Info ────────────────────────────────────────────────── */

/* dirent hash table: 64 buckets (shift=6) covers most directories well.
 * Allocated lazily on first agfs_add_dirent; NULL for leaf files. */
#define AGFS_DE_SHIFT		10
#define AGFS_DE_BUCKETS	(1u << AGFS_DE_SHIFT)

struct agfs_inode_info {
	struct inode		*lower_inode;
	enum agfs_perm		cached_perm;
	u64			perm_gen;

	/* Directory dirent table (lazily allocated, protected by VFS i_rwsem) */
	struct hlist_head	*de_buckets;	/* NULL until first dirent */
	struct list_head	de_pin;	/* node in sbi->pinned_dirs */

	struct inode		vfs_inode;	/* must be last for container_of */
};

/* ── Per-Dentry Info ───────────────────────────────────────────────── */

struct agfs_dentry_info {
	spinlock_t		lock;
	struct path		lower_path;	/* resolved lower path (inode entry or base) */
	enum agfs_perm		perm;		/* NONE unless explicit rule */
	struct list_head	rule_pin;	/* node in sbi->pinned_rules */
	struct dentry		*rule_dentry;	/* back-pointer for dput on release */
};

/* ── Per-File Info ─────────────────────────────────────────────────── */

struct agfs_file_info {
	struct file		*lower_file;
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

static inline struct agfs_file_info *AGFS_F(const struct file *file)
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

static inline struct super_block *agfs_lower_super(const struct super_block *sb)
{
	return AGFS_SB(sb)->lower_sb;
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
extern const struct file_operations agfs_dir_fops;
extern const struct address_space_operations agfs_aops;

/* dentry.c */
extern const struct dentry_operations agfs_dops;
extern const struct dentry_operations agfs_dops_fast;
int agfs_init_dentry_cache(void);
void agfs_destroy_dentry_cache(void);
int agfs_new_dentry_private_data(struct dentry *dentry);
void agfs_free_dentry_private_data(struct dentry *dentry);

/* lookup.c */
struct dentry *agfs_lookup(struct inode *dir, struct dentry *dentry,
			   unsigned int flags);
struct inode *agfs_iget(struct super_block *sb, struct inode *lower_inode);
int agfs_interpose(struct dentry *dentry, struct super_block *sb,
		   struct path *lower_path);

/* staging.c */
int agfs_dentry_relpath(struct dentry *dentry, char *buf, int buflen);
int agfs_inode_path(struct agfs_sb_info *sbi, u64 ino,
		    struct path *result);
struct agfs_dirent *agfs_find_dirent(struct inode *dir,
					 const char *name,
					 unsigned int namelen);
int agfs_add_dirent(struct inode *dir, const char *name,
		      unsigned int namelen,
		      const struct agfs_dirent *de);
int agfs_del_dirent(struct inode *dir, const char *name,
		      unsigned int namelen);
int agfs_inode_alloc(struct agfs_sb_info *sbi, u64 *out_ino,
		     struct path *inode_path, umode_t mode,
		     const char *symname);
int agfs_do_cow(struct agfs_sb_info *sbi, struct dentry *dentry,
		struct file **new_file, int flags, bool truncate);
void agfs_release_pinned_dirs(struct agfs_sb_info *sbi);

/* journal.c */
int agfs_journal_open(struct agfs_sb_info *sbi);
int agfs_journal_add(struct agfs_sb_info *sbi, struct dentry *dentry,
		       u64 ino, unsigned char d_type);
int agfs_journal_modify(struct agfs_sb_info *sbi, struct dentry *dentry,
			  u64 ino, unsigned char d_type);
int agfs_journal_delete(struct agfs_sb_info *sbi, struct dentry *dentry);
int agfs_journal_redirect(struct agfs_sb_info *sbi, struct dentry *old_dentry,
			    struct dentry *new_dentry, unsigned char d_type);
int agfs_journal_replace(struct agfs_sb_info *sbi, struct dentry *old_dentry,
			   struct dentry *new_dentry, unsigned char d_type);
int agfs_journal_checkpoint(struct agfs_sb_info *sbi, u64 id, const char *name);
int agfs_journal_restore(struct agfs_sb_info *sbi, u64 gen, u64 target_gen);

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
