// SPDX-License-Identifier: GPL-2.0
/*
 * yolofs — superblock operations and module init/exit.
 */

#include "yolofs.h"
#include <linux/fs_context.h>
#include <linux/fs_parser.h>
#include <linux/statfs.h>
#include <linux/seq_file.h>

/* ── Mount Options ─────────────────────────────────────────────────── */

enum yolo_param {
	Opt_permission,
	Opt_staging,
	Opt_ask_timeout,
	Opt_ask_default,
};

static const struct fs_parameter_spec yolo_fs_parameters[] = {
	fsparam_bool("permission",	Opt_permission),
	fsparam_bool("staging",		Opt_staging),
	fsparam_u32("ask_timeout",	Opt_ask_timeout),
	fsparam_u32("ask_default",	Opt_ask_default),
	{}
};

struct yolo_fs_opts {
	bool		permission;
	bool		staging;
	unsigned int	ask_timeout_s;
	unsigned int	ask_default;
};

/* ── Inode Slab Cache ──────────────────────────────────────────────── */

struct kmem_cache *yolo_inode_cachep;

static struct inode *yolo_alloc_inode(struct super_block *sb)
{
	struct yolo_inode_info *i;

	i = alloc_inode_sb(sb, yolo_inode_cachep, GFP_KERNEL);
	if (!i)
		return NULL;

	i->lower_inode = NULL;
	i->cached_perm = YOLO_PERM_NONE;
	i->perm_gen = 0;
	i->staging_gen = 0;
	return &i->vfs_inode;
}

static void yolo_free_inode(struct inode *inode)
{
	kmem_cache_free(yolo_inode_cachep, YOLO_I(inode));
}

static void yolo_evict_inode(struct inode *inode)
{
	struct inode *lower_inode;

	truncate_inode_pages(&inode->i_data, 0);
	clear_inode(inode);

	lower_inode = yolo_lower_inode(inode);
	if (lower_inode)
		iput(lower_inode);
}

/* ── Superblock Ops ────────────────────────────────────────────────── */

static void yolo_put_super(struct super_block *sb)
{
	struct yolo_sb_info *sbi = YOLO_SB(sb);

	if (!sbi)
		return;

	if (sbi->ctl_inode)
		iput(sbi->ctl_inode);
	if (sbi->shard_dentry)
		dput(sbi->shard_dentry);
	if (sbi->inodes_dir.dentry)
		path_put(&sbi->inodes_dir);
	if (sbi->journal_file)
		fput(sbi->journal_file);
	if (sbi->storage_path.dentry)
		path_put(&sbi->storage_path);
	if (sbi->base_path.dentry)
		path_put(&sbi->base_path);
	if (sbi->lower_sb)
		atomic_dec(&sbi->lower_sb->s_active);

	kfree(sbi);
	sb->s_fs_info = NULL;
}

static int yolo_statfs(struct dentry *dentry, struct kstatfs *buf)
{
	struct path lower_path;
	int err;

	yolo_get_lower_path(dentry, &lower_path);
	err = vfs_statfs(&lower_path, buf);
	yolo_put_lower_path(dentry, &lower_path);

	if (!err)
		buf->f_type = YOLO_SUPER_MAGIC;
	return err;
}

static int yolo_show_options(struct seq_file *m, struct dentry *root)
{
	struct yolo_sb_info *sbi = YOLO_SB(root->d_sb);

	seq_printf(m, ",permission=%d", sbi->permission);
	seq_printf(m, ",staging=%d", sbi->staging);
	seq_printf(m, ",ask_timeout=%u", sbi->ask_engine.timeout_s);
	seq_printf(m, ",ask_default=%d", sbi->ask_engine.default_perm);
	return 0;
}

const struct super_operations yolo_sops = {
	.alloc_inode	= yolo_alloc_inode,
	.free_inode	= yolo_free_inode,
	.evict_inode	= yolo_evict_inode,
	.put_super	= yolo_put_super,
	.statfs		= yolo_statfs,
	.show_options	= yolo_show_options,
};

/* ── Fill Superblock helpers ────────────────────────────────────────── */

/*
 * Create the synthetic .ctl control file inode and pin its dentry
 * in the dcache.  Must be called after sb->s_root is set up.
 */
static int yolo_init_ctl(struct super_block *sb)
{
	struct yolo_sb_info *sbi = YOLO_SB(sb);
	struct inode *ctl;
	struct dentry *ctl_dentry;
	struct qstr ctl_name = QSTR_INIT(".ctl", 4);

	ctl = new_inode(sb);
	if (!ctl)
		return -ENOMEM;

	ctl->i_ino = get_next_ino();
	ctl->i_mode = S_IFREG | 0600;
	ctl->i_uid = current_uid();
	ctl->i_gid = current_gid();
	ctl->i_fop = &yolo_ctl_fops;
	simple_inode_init_ts(ctl);
	sbi->ctl_inode = ctl;

	/* Pin .ctl in the dcache so lookup_fast() always finds it */
	ctl_name.hash = full_name_hash(sb->s_root, ".ctl", 4);
	ctl_dentry = d_alloc(sb->s_root, &ctl_name);
	if (!ctl_dentry)
		return -ENOMEM;

	ihold(ctl);
	d_add(ctl_dentry, ctl);
	sbi->ctl_dentry = ctl_dentry;
	return 0;
}

static void yolo_init_sbi(struct yolo_sb_info *sbi,
			   const struct yolo_fs_opts *opts)
{
	sbi->permission = opts->permission;
	sbi->staging = opts->staging;

	/* Permission gating state */
	atomic64_set(&sbi->perm_gen, 1);
	INIT_LIST_HEAD(&sbi->ask_engine.pending_reqs);
	spin_lock_init(&sbi->ask_engine.pending_lock);
	init_waitqueue_head(&sbi->ask_engine.request_waitq);
	atomic64_set(&sbi->ask_engine.next_req_id, 1);
	INIT_LIST_HEAD(&sbi->ask_engine.dispatched);
	spin_lock_init(&sbi->ask_engine.dispatch_lock);
	sbi->ask_engine.timeout_s = opts->ask_timeout_s;
	sbi->ask_engine.default_perm = opts->ask_default
		? opts->ask_default : YOLO_PERM_DENY;
	INIT_LIST_HEAD(&sbi->pinned_rules);
	spin_lock_init(&sbi->pinned_rules_lock);

	/* Staging state */
	init_rwsem(&sbi->staging_sem);
	atomic_set(&sbi->next_ino, 0);
	atomic_set(&sbi->gen, 0);
	atomic_set(&sbi->staging_fd_count, 0);
}

static int yolo_resolve_paths(struct yolo_sb_info *sbi,
			      struct super_block *sb,
			      struct fs_context *fc)
{
	struct path base_path;
	int err;

	/* Resolve base path ("/") */
	err = kern_path("/", LOOKUP_FOLLOW | LOOKUP_DIRECTORY, &base_path);
	if (err)
		return err;

	sbi->base_path = base_path;
	sbi->lower_sb = base_path.dentry->d_sb;
	atomic_inc(&sbi->lower_sb->s_active);
	sb->s_maxbytes = sbi->lower_sb->s_maxbytes;
	sb->s_stack_depth = sbi->lower_sb->s_stack_depth + 1;
	if (sb->s_stack_depth > FILESYSTEM_MAX_STACK_DEPTH)
		return -EINVAL;

	/* Resolve storage path from mount source (required) */
	if (!fc->source || !fc->source[0]) {
		pr_err("yolofs: source path is required\n");
		return -EINVAL;
	}

	err = kern_path(fc->source, LOOKUP_FOLLOW | LOOKUP_DIRECTORY,
			&sbi->storage_path);
	if (err)
		return err;

	/* Resolve inodes dir (may not exist yet — that's ok) */
	{
		struct path inodes;

		err = vfs_path_lookup(sbi->storage_path.dentry,
				      sbi->storage_path.mnt,
				      "inodes", LOOKUP_DIRECTORY,
				      &inodes);
		if (!err)
			sbi->inodes_dir = inodes;
	}

	/* Open the journal file */
	err = yolo_journal_open(sbi);
	if (err)
		return err;

	return 0;
}

/* ── Fill Superblock (mount) ───────────────────────────────────────── */

static int yolo_fill_super(struct super_block *sb, struct fs_context *fc)
{
	struct yolo_fs_opts *opts = fc->fs_private;
	struct yolo_sb_info *sbi;
	struct inode *inode;
	int err;

	sbi = kzalloc(sizeof(*sbi), GFP_KERNEL);
	if (!sbi)
		return -ENOMEM;

	sb->s_fs_info = sbi;
	sb->s_op = &yolo_sops;
	sb->s_magic = YOLO_SUPER_MAGIC;
	sb->s_maxbytes = MAX_LFS_FILESIZE;
	sb->s_stack_depth = 0;

	yolo_init_sbi(sbi, opts);

	err = yolo_resolve_paths(sbi, sb, fc);
	if (err)
		goto out_put;

	/*
	 * Reject lower filesystems that need d_revalidate (e.g. NFS).
	 * YoloFS only supports local filesystems (ext4, xfs, btrfs, …).
	 */
	if (sbi->base_path.dentry->d_flags & DCACHE_OP_REVALIDATE) {
		pr_err("yolofs: lower filesystem requires d_revalidate; "
		       "only local filesystems are supported\n");
		err = -EINVAL;
		goto out_put;
	}
	sb->s_d_op = &yolo_dops;

	/* Create root inode from lower root */
	inode = yolo_iget(sb, d_inode(sbi->base_path.dentry));
	if (IS_ERR(inode)) {
		err = PTR_ERR(inode);
		goto out_put;
	}

	sb->s_root = d_make_root(inode);
	if (!sb->s_root) {
		err = -ENOMEM;
		goto out_put;
	}

	/* d_init already allocated d_fsdata for the root dentry */

	path_get(&sbi->base_path);
	yolo_set_lower_path(sb->s_root, &sbi->base_path);

	YOLO_D(sb->s_root)->perm = YOLO_PERM_ASK;

	err = yolo_init_ctl(sb);
	if (err)
		goto out_put;

	return 0;

out_put:
	yolo_put_super(sb);
	return err;
}

/* ── fs_context operations ─────────────────────────────────────────── */

static int yolo_parse_param(struct fs_context *fc, struct fs_parameter *param)
{
	struct yolo_fs_opts *opts = fc->fs_private;
	struct fs_parse_result result;
	int opt;

	opt = fs_parse(fc, yolo_fs_parameters, param, &result);
	if (opt < 0)
		return opt;

	switch (opt) {
	case Opt_permission:
		opts->permission = result.boolean;
		break;
	case Opt_staging:
		opts->staging = result.boolean;
		break;
	case Opt_ask_timeout:
		opts->ask_timeout_s = result.uint_32;
		break;
	case Opt_ask_default:
		opts->ask_default = result.uint_32;
		break;
	default:
		return -EINVAL;
	}
	return 0;
}

static int yolo_get_tree(struct fs_context *fc)
{
	return get_tree_nodev(fc, yolo_fill_super);
}

static void yolo_free_fc(struct fs_context *fc)
{
	kfree(fc->fs_private);
}

static const struct fs_context_operations yolo_context_ops = {
	.parse_param	= yolo_parse_param,
	.get_tree	= yolo_get_tree,
	.free		= yolo_free_fc,
};

static int yolo_init_fs_context(struct fs_context *fc)
{
	struct yolo_fs_opts *opts;

	opts = kzalloc(sizeof(*opts), GFP_KERNEL);
	if (!opts)
		return -ENOMEM;

	fc->fs_private = opts;
	fc->ops = &yolo_context_ops;
	return 0;
}

static void yolo_kill_super(struct super_block *sb)
{
	struct yolo_sb_info *sbi = YOLO_SB(sb);

	if (sbi) {
		yolo_release_pinned_rules(sbi);
		if (sbi->ctl_dentry) {
			dput(sbi->ctl_dentry);
			sbi->ctl_dentry = NULL;
		}
		yolo_dentry_unpin_all(sb);
	}

	kill_anon_super(sb);
}

/* ── Filesystem Type & Module ──────────────────────────────────────── */

static struct file_system_type yolo_fs_type = {
	.owner			= THIS_MODULE,
	.name			= "yolofs",
	.init_fs_context	= yolo_init_fs_context,
	.kill_sb		= yolo_kill_super,
	.fs_flags		= FS_USERNS_MOUNT,
};
MODULE_ALIAS_FS("yolofs");

static void yolo_inode_init_once(void *obj)
{
	struct yolo_inode_info *i = obj;
	inode_init_once(&i->vfs_inode);
}

static int __init yolo_init(void)
{
	int err;

	BUILD_BUG_ON(ARCH_KMALLOC_MINALIGN < 8);
	BUILD_BUG_ON(!IS_ENABLED(CONFIG_X86_64));

	yolo_inode_cachep = kmem_cache_create("yolo_inode_cache",
					      sizeof(struct yolo_inode_info), 0,
					      SLAB_RECLAIM_ACCOUNT | SLAB_ACCOUNT,
					      yolo_inode_init_once);
	if (!yolo_inode_cachep)
		return -ENOMEM;

	err = yolo_init_dentry_cache();
	if (err)
		goto out_inode;

	err = register_filesystem(&yolo_fs_type);
	if (err)
		goto out_dentry;

	pr_info("yolofs: module loaded\n");
	return 0;

out_dentry:
	yolo_destroy_dentry_cache();
out_inode:
	kmem_cache_destroy(yolo_inode_cachep);
	return err;
}

static void __exit yolo_exit(void)
{
	unregister_filesystem(&yolo_fs_type);

	/* Wait for all pending RCU callbacks (the VFS defers free_inode
	 * via call_rcu, so slabs may still be in use until those fire). */
	rcu_barrier();

	yolo_destroy_dentry_cache();
	kmem_cache_destroy(yolo_inode_cachep);
	pr_info("yolofs: module unloaded\n");
}

module_init(yolo_init);
module_exit(yolo_exit);

MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("yolofs — agentic filesystem with staging-commit and permission gating");
