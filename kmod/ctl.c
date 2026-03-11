// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — control file (./agfs/ctl).
 *
 * Binary read/write/poll/ioctl interface for permission daemon.
 *
 * Request lifecycle:
 *   pending  → read() dequeues, moves to dispatched list
 *   dispatched → write() finds by id, sets decision, completes
 *   on ctl release → all dispatched requests get default decision
 */

#include "agfs.h"
#include <linux/poll.h>

/* Per-open-file state: track dispatched requests so we can clean up
 * if the daemon closes the ctl fd without responding. */
struct agfs_ctl_private {
	struct list_head	dispatched;	/* requests sent to this fd */
	spinlock_t		lock;
};

static int agfs_ctl_open(struct inode *inode, struct file *file)
{
	struct agfs_ctl_private *priv;

	priv = kzalloc(sizeof(*priv), GFP_KERNEL);
	if (!priv)
		return -ENOMEM;
	INIT_LIST_HEAD(&priv->dispatched);
	spin_lock_init(&priv->lock);
	file->private_data = priv;
	return 0;
}

/* ── read: dequeue pending request → dispatched list ───────────────── */

static ssize_t agfs_ctl_read(struct file *file, char __user *buf,
			     size_t count, loff_t *ppos)
{
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);
	struct agfs_ctl_private *priv = file->private_data;
	struct agfs_perm_request *req;
	struct agfs_ctl_request out;
	int err;

	if (count < sizeof(out))
		return -EINVAL;

	/* Wait for a pending request */
	if (file->f_flags & O_NONBLOCK) {
		spin_lock(&sbi->pending_lock);
		if (list_empty(&sbi->pending_reqs)) {
			spin_unlock(&sbi->pending_lock);
			return -EAGAIN;
		}
	} else {
		err = wait_event_interruptible(sbi->request_waitq,
			!list_empty(&sbi->pending_reqs));
		if (err)
			return err;
		spin_lock(&sbi->pending_lock);
		if (list_empty(&sbi->pending_reqs)) {
			spin_unlock(&sbi->pending_lock);
			return -EAGAIN;
		}
	}

	/* Dequeue oldest request */
	req = list_first_entry(&sbi->pending_reqs,
			       struct agfs_perm_request, list);
	list_del_init(&req->list);
	spin_unlock(&sbi->pending_lock);

	/* Fill output struct */
	memset(&out, 0, sizeof(out));
	out.id = req->id;
	out.op = req->op;
	out.pid = req->pid;
	strscpy(out.comm, req->comm, sizeof(out.comm));
	strscpy(out.path, req->path, sizeof(out.path));

	/* Move to this fd's dispatched list */
	spin_lock(&priv->lock);
	list_add_tail(&req->list, &priv->dispatched);
	spin_unlock(&priv->lock);

	if (copy_to_user(buf, &out, sizeof(out))) {
		/* Move back to pending so another read can pick it up */
		spin_lock(&priv->lock);
		list_del_init(&req->list);
		spin_unlock(&priv->lock);

		spin_lock(&sbi->pending_lock);
		list_add(&req->list, &sbi->pending_reqs);
		spin_unlock(&sbi->pending_lock);
		wake_up_interruptible(&sbi->request_waitq);
		return -EFAULT;
	}

	return sizeof(out);
}

/* ── write: submit decision for a dispatched request ───────────────── */

static ssize_t agfs_ctl_write(struct file *file, const char __user *buf,
			      size_t count, loff_t *ppos)
{
	struct agfs_ctl_private *priv = file->private_data;
	struct agfs_ctl_response in;
	struct agfs_perm_request *req, *tmp;
	bool found = false;

	if (count < sizeof(in))
		return -EINVAL;

	if (copy_from_user(&in, buf, sizeof(in)))
		return -EFAULT;

	/* Find request by id in dispatched list */
	spin_lock(&priv->lock);
	list_for_each_entry_safe(req, tmp, &priv->dispatched, list) {
		if (req->id == in.id) {
			req->decision = (enum agfs_perm)in.decision;
			list_del_init(&req->list);
			found = true;
			break;
		}
	}
	spin_unlock(&priv->lock);

	if (!found)
		return -ENOENT;

	/* Wake the sleeping thread */
	complete(&req->done);
	return sizeof(in);
}

/* ── release: complete all orphaned dispatched requests ────────────── */

static int agfs_ctl_release(struct inode *inode, struct file *file)
{
	struct agfs_sb_info *sbi = AGFS_SB(inode->i_sb);
	struct agfs_ctl_private *priv = file->private_data;
	struct agfs_perm_request *req, *tmp;

	if (!priv)
		return 0;

	/* Complete any dispatched requests the daemon never answered */
	spin_lock(&priv->lock);
	list_for_each_entry_safe(req, tmp, &priv->dispatched, list) {
		req->decision = sbi->ask_default;
		list_del_init(&req->list);
		complete(&req->done);
	}
	spin_unlock(&priv->lock);

	kfree(priv);
	file->private_data = NULL;
	return 0;
}

/* ── poll ──────────────────────────────────────────────────────────── */

static __poll_t agfs_ctl_poll(struct file *file,
			      struct poll_table_struct *wait)
{
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);
	__poll_t mask = 0;

	poll_wait(file, &sbi->request_waitq, wait);

	spin_lock(&sbi->pending_lock);
	if (!list_empty(&sbi->pending_reqs))
		mask |= EPOLLIN | EPOLLRDNORM;
	spin_unlock(&sbi->pending_lock);

	/* Always writable */
	mask |= EPOLLOUT | EPOLLWRNORM;
	return mask;
}

/* ── ioctl ─────────────────────────────────────────────────────────── */

static long agfs_ctl_ioctl(struct file *file, unsigned int cmd,
			   unsigned long arg)
{
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);

	switch (cmd) {
	case AGFS_IOC_RULE_ADD: {
		struct agfs_ioc_rule rule;
		struct path rule_path;
		struct agfs_dentry_info *di;
		int err;

		if (copy_from_user(&rule, (void __user *)arg, sizeof(rule)))
			return -EFAULT;

		rule.path[AGFS_PATH_MAX - 1] = '\0';

		/* Resolve path to dentry — must be on this agfs instance */
		err = kern_path(rule.path, LOOKUP_FOLLOW, &rule_path);
		if (err)
			return err;

		if (rule_path.dentry->d_sb != file_inode(file)->i_sb) {
			path_put(&rule_path);
			return -EXDEV;
		}

		di = AGFS_D(rule_path.dentry);
		if (!di) {
			path_put(&rule_path);
			return -ENOENT;
		}

		spin_lock(&di->lock);
		di->perm = (enum agfs_perm)rule.perm;
		spin_unlock(&di->lock);

		/* Pin the dentry so it's not evicted */
		dget(rule_path.dentry);

		/* Bump generation to invalidate all cached perms */
		atomic64_inc(&sbi->perm_gen);
		path_put(&rule_path);

		agfs_log_emit(sbi, AGFS_LOG_RULE, rule.perm, 0,
			      rule.path, 0);
		return 0;
	}

	case AGFS_IOC_RULE_REMOVE: {
		struct agfs_ioc_rule rule;
		struct path rule_path;
		struct agfs_dentry_info *di;
		int err;

		if (copy_from_user(&rule, (void __user *)arg, sizeof(rule)))
			return -EFAULT;

		rule.path[AGFS_PATH_MAX - 1] = '\0';

		err = kern_path(rule.path, LOOKUP_FOLLOW, &rule_path);
		if (err)
			return err;

		if (rule_path.dentry->d_sb != file_inode(file)->i_sb) {
			path_put(&rule_path);
			return -EXDEV;
		}

		di = AGFS_D(rule_path.dentry);
		if (!di) {
			path_put(&rule_path);
			return -ENOENT;
		}

		spin_lock(&di->lock);
		di->perm = AGFS_PERM_NONE;
		spin_unlock(&di->lock);

		/* Unpin the dentry */
		dput(rule_path.dentry);

		atomic64_inc(&sbi->perm_gen);
		path_put(&rule_path);
		return 0;
	}

	case AGFS_IOC_CACHE_INVAL:
		atomic64_inc(&sbi->perm_gen);
		agfs_log_emit(sbi, AGFS_LOG_COMMIT, 0, 0, "", 0);
		return 0;

	default:
		return -ENOTTY;
	}
}

/* ── File Ops ──────────────────────────────────────────────────────── */

const struct file_operations agfs_ctl_fops = {
	.owner		= THIS_MODULE,
	.open		= agfs_ctl_open,
	.release	= agfs_ctl_release,
	.read		= agfs_ctl_read,
	.write		= agfs_ctl_write,
	.poll		= agfs_ctl_poll,
	.unlocked_ioctl	= agfs_ctl_ioctl,
	.compat_ioctl	= agfs_ctl_ioctl,
};
