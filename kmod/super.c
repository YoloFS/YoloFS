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
	Opt_prompt_timeout_ms,
};

static const struct fs_parameter_spec yolo_fs_parameters[] = {
	fsparam_bool("permission",	Opt_permission),
	fsparam_bool("staging",		Opt_staging),
	fsparam_u32("prompt_timeout_ms",	Opt_prompt_timeout_ms),
	{}
};

struct yolo_fs_opts {
	bool		permission;
	bool		staging;
	unsigned int	prompt_timeout_ms;
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
	i->staging_gen = 0;
	i->staging_ino = 0;
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

	if (sbi->staging.shard_dentry)
		dput(sbi->staging.shard_dentry);
	if (sbi->staging.inodes_dir.dentry)
		path_put(&sbi->staging.inodes_dir);
	if (sbi->staging.journal_file)
		fput(sbi->staging.journal_file);
	if (sbi->storage_path.dentry)
		path_put(&sbi->storage_path);
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

	seq_printf(m, ",permission=%d", sbi->perm.enabled);
	seq_printf(m, ",staging=%d", sbi->staging.enabled);
	seq_printf(m, ",prompt_timeout_ms=%u", sbi->perm.timeout_ms);
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

static void yolo_init_sbi(struct yolo_sb_info *sbi,
			   const struct yolo_fs_opts *opts)
{
	sbi->perm.enabled = opts->permission;
	sbi->staging.enabled = opts->staging;

	/* Permission gating state */
	INIT_LIST_HEAD(&sbi->perm.pending_reqs);
	spin_lock_init(&sbi->perm.pending_lock);
	init_waitqueue_head(&sbi->perm.request_waitq);
	atomic64_set(&sbi->perm.next_req_id, 1);
	sbi->perm.timeout_ms = opts->prompt_timeout_ms;
	INIT_LIST_HEAD(&sbi->perm.pinned_rules);
	spin_lock_init(&sbi->perm.pinned_rules_lock);
	mutex_init(&sbi->perm.update_lock);

	/* Staging state */
	init_rwsem(&sbi->staging.sem);
	spin_lock_init(&sbi->staging.shard_lock);
	atomic_set(&sbi->staging.next_ino, 0);
	atomic_set(&sbi->staging.gen, 0);
	atomic_set(&sbi->staging.fd_count, 0);
}

/*
 * Resolve the lower paths. The lower root ("/") is returned to the caller via
 * @base_path (it is only needed during mount, to build the root dentry); the
 * caller owns that reference and must path_put it. lower_sb keeps the lower
 * superblock alive for our lifetime, and the root dentry pins the lower root.
 */
static int yolo_resolve_paths(struct yolo_sb_info *sbi,
			      struct super_block *sb,
			      struct fs_context *fc,
			      struct path *base_path)
{
	int err;

	/* Resolve base path ("/") */
	err = kern_path("/", LOOKUP_FOLLOW | LOOKUP_DIRECTORY, base_path);
	if (err)
		return err;

	sbi->lower_sb = base_path->dentry->d_sb;
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
			sbi->staging.inodes_dir = inodes;
	}

	/* Open the journal file */
	{
		struct file *f = yolo_journal_open(&sbi->storage_path);

		if (IS_ERR(f))
			return PTR_ERR(f);
		sbi->staging.journal_file = f;
	}

	return 0;
}

/* ── Fill Superblock (mount) ───────────────────────────────────────── */

static int yolo_fill_super(struct super_block *sb, struct fs_context *fc)
{
	struct yolo_fs_opts *opts = fc->fs_private;
	struct yolo_sb_info *sbi;
	struct path base_path = {};
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

	err = yolo_resolve_paths(sbi, sb, fc, &base_path);
	if (err)
		goto out_put;

	/* sb->s_d_op removed in favour of set_default_d_op() in 7.0 */
#if LINUX_VERSION_CODE >= KERNEL_VERSION(7, 0, 0)
	set_default_d_op(sb, &yolo_dops);
#else
	sb->s_d_op = &yolo_dops;
#endif

	/* Create root inode from lower root */
	inode = yolo_iget(sb, d_inode(base_path.dentry));
	if (IS_ERR(inode)) {
		err = PTR_ERR(inode);
		goto out_put;
	}

	sb->s_root = d_make_root(inode);
	if (!sb->s_root) {
		err = -ENOMEM;
		goto out_put;
	}

	/* d_init already allocated d_fsdata for the root dentry. The root dentry
	 * takes its own reference on the lower root, so we drop ours below. */
	path_get(&base_path);
	yolo_set_lower_path(sb->s_root, &base_path);

	path_put(&base_path);
	return 0;

out_put:
	/* Safe on a zero-initialized path if resolve_paths never acquired it. */
	path_put(&base_path);
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
	case Opt_prompt_timeout_ms:
		opts->prompt_timeout_ms = result.uint_32;
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
