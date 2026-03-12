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
#include <linux/poll.h>
#include <linux/ioctl.h>
#include <linux/ktime.h>
#include <linux/uaccess.h>
#include <linux/magic.h>
#include <linux/module.h>

/* ── Constants ─────────────────────────────────────────────────────── */

#define AGFS_SUPER_MAGIC	0xA6F5
#define AGFS_PATH_MAX		256
#define AGFS_LOG_DEFAULT_SIZE	1024

/* Operations passed in ask requests / log entries */
#define AGFS_OP_READ		1
#define AGFS_OP_WRITE		2
#define AGFS_OP_EXEC		3
#define AGFS_OP_OPEN		4
#define AGFS_OP_LOOKUP		5

/* Log event types */
#define AGFS_LOG_OPEN		1
#define AGFS_LOG_ASK		2
#define AGFS_LOG_DECISION	3
#define AGFS_LOG_DENY		4
#define AGFS_LOG_COW		5
#define AGFS_LOG_RULE		6
#define AGFS_LOG_COMMIT		7
#define AGFS_LOG_ABORT		8

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

struct agfs_ioc_rule {
	char	path[AGFS_PATH_MAX];
	__u8	perm;			/* enum agfs_perm value */
	__u8	_pad[7];
};

#define AGFS_IOC_RULE_ADD	_IOW('A', 10, struct agfs_ioc_rule)
#define AGFS_IOC_RULE_REMOVE	_IOW('A', 11, struct agfs_ioc_rule)
#define AGFS_IOC_CACHE_INVAL	_IO('A', 20)
#define AGFS_IOC_CTL_READ	_IOR('A', 30, struct agfs_ctl_request)
#define AGFS_IOC_CTL_WRITE	_IOW('A', 31, struct agfs_ctl_response)

/* ── Control-File Protocol (binary, fixed-size) ────────────────────── */

/* kernel → userspace: read() returns one of these */
struct agfs_ctl_request {
	__u64	id;
	__u32	op;			/* AGFS_OP_READ / WRITE / EXEC */
	__u32	pid;
	char	comm[16];
	char	path[AGFS_PATH_MAX];
};

/* userspace → kernel: write() accepts one of these */
struct agfs_ctl_response {
	__u64	id;
	__u8	decision;		/* enum agfs_perm value */
	__u8	_pad[7];
};

/* ── Log Entry ─────────────────────────────────────────────────────── */

struct agfs_log_entry {
	__u64	timestamp_ns;
	__u64	req_id;
	__u32	op;
	__u32	pid;
	__u8	event;			/* AGFS_LOG_* */
	__u8	perm;			/* enum agfs_perm result */
	__u16	_pad;
	char	comm[16];
	char	path[AGFS_PATH_MAX];
};

/* ── Internal: Pending Permission Request ──────────────────────────── */

struct agfs_perm_request {
	u64			id;
	char			path[AGFS_PATH_MAX];
	unsigned int		op;
	pid_t			pid;
	char			comm[TASK_COMM_LEN];

	enum agfs_perm		decision;
	struct completion	done;
	struct list_head	list;
};

/* ── Log Ring Buffer ───────────────────────────────────────────────── */

struct agfs_log_ring {
	struct agfs_log_entry	*entries;
	unsigned int		size;		/* number of slots */
	unsigned int		head;		/* next write position */
	unsigned int		count;		/* entries available to read */
	spinlock_t		lock;
	wait_queue_head_t	waitq;
};

/* ── Pinned Dentry (for rename tracking) ────────────────────────────── */

struct agfs_pinned_dentry {
	struct dentry		*dentry;
	struct list_head	list;
};

/* ── Per-Superblock Info ───────────────────────────────────────────── */

struct agfs_sb_info {
	struct super_block	*lower_sb;
	struct path		base_path;	/* always "/" */
	struct path		storage_path;	/* ./agfs/ directory */
	const struct cred	*creator_cred;	/* mount-time credentials */

	/* Staging */
	struct path		staging_dir;	/* ./agfs/staging/ */
	struct path		renames_path;	/* ./agfs/renames */
	struct rw_semaphore	staging_sem;
	struct list_head	pinned_dentries;/* rename-pinned dentries */

	/* Permission gating */
	atomic64_t		perm_gen;
	struct list_head	pending_reqs;
	spinlock_t		pending_lock;
	wait_queue_head_t	request_waitq;
	atomic64_t		next_req_id;
	unsigned int		ask_timeout_s;
	enum agfs_perm		ask_default;
	bool			noperm;
	bool			nostaging;

	/* Log */
	struct agfs_log_ring	*log;
	unsigned int		log_size;
};

/* ── Per-Inode Info ────────────────────────────────────────────────── */

struct agfs_inode_info {
	struct inode		*lower_inode;
	enum agfs_perm		cached_perm;
	u64			perm_gen;
	struct inode		vfs_inode;	/* must be last for container_of */
};

/* ── Per-Dentry Info ───────────────────────────────────────────────── */

struct agfs_dentry_info {
	spinlock_t		lock;
	struct path		lower_path;
	enum agfs_perm		perm;		/* NONE unless explicit rule */
};

/* ── Per-File Ctl State (for permission daemon fds) ─────────────────── */

struct agfs_ctl_private {
	struct list_head	dispatched;	/* requests sent to this fd */
	spinlock_t		lock;
};

/* ── Per-File Info ─────────────────────────────────────────────────── */

struct agfs_file_info {
	struct file		*lower_file;
	bool			needs_cow;
	bool			is_staging;
	const struct vm_operations_struct *lower_vm_ops;
	struct agfs_ctl_private	*ctl;	/* non-NULL if this fd is a ctl daemon */
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
int agfs_staging_path(struct agfs_sb_info *sbi, const char *relpath,
		      struct path *result);
int agfs_base_path(struct agfs_sb_info *sbi, const char *relpath,
		   struct path *result);
int agfs_resolve_lower(struct dentry *dentry, struct path *result);
bool agfs_staging_has(struct agfs_sb_info *sbi, const char *relpath);
bool agfs_is_whiteout(struct dentry *dentry);
int agfs_create_whiteout(struct agfs_sb_info *sbi, const char *relpath);
int agfs_do_cow(struct agfs_sb_info *sbi, const char *relpath,
		struct file **new_file, int flags);
int agfs_create_staging_empty(struct agfs_sb_info *sbi, const char *relpath,
			      struct file **new_file, int flags);
int agfs_create_staging_parents(struct agfs_sb_info *sbi, const char *relpath);
int agfs_dentry_relpath(struct dentry *dentry, char *buf, int buflen);
int agfs_append_rename(struct agfs_sb_info *sbi,
		       const char *old_path, const char *new_path);

/* perm.c */
enum agfs_perm agfs_resolve_perm(struct dentry *dentry);
void agfs_cache_perm(struct inode *inode, struct dentry *dentry);
int agfs_check_perm(enum agfs_perm perm, int f_flags);
int agfs_ask_userspace(struct agfs_sb_info *sbi, struct dentry *dentry,
		       const char *relpath, int f_flags,
		       enum agfs_perm *result);

/* ioctl.c */
long agfs_ioctl(struct file *file, unsigned int cmd, unsigned long arg);
__poll_t agfs_ctl_poll(struct file *file, struct poll_table_struct *wait);
void agfs_ctl_cleanup(struct agfs_sb_info *sbi, struct agfs_ctl_private *priv);

/* log.c */
extern const struct file_operations agfs_log_fops;
int agfs_log_init(struct agfs_sb_info *sbi);
void agfs_log_destroy(struct agfs_sb_info *sbi);
void agfs_log_emit(struct agfs_sb_info *sbi, u8 event, u8 perm,
		   u32 op, const char *path, u64 req_id);

#endif /* _AGFS_H_ */
