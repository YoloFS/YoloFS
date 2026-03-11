// SPDX-License-Identifier: GPL-2.0
/*
 * agfs — log file (./agfs/log).
 *
 * Ring buffer of structured binary log entries. Kernel writes entries;
 * userspace reads them from ./agfs/log.
 */

#include "agfs.h"
#include <linux/poll.h>

/* ── Initialize log ring buffer ────────────────────────────────────── */

int agfs_log_init(struct agfs_sb_info *sbi)
{
	struct agfs_log_ring *ring;

	ring = kzalloc(sizeof(*ring), GFP_KERNEL);
	if (!ring)
		return -ENOMEM;

	ring->size = sbi->log_size ? sbi->log_size : AGFS_LOG_DEFAULT_SIZE;
	ring->entries = kvcalloc(ring->size, sizeof(struct agfs_log_entry),
				 GFP_KERNEL);
	if (!ring->entries) {
		kfree(ring);
		return -ENOMEM;
	}

	ring->head = 0;
	ring->count = 0;
	spin_lock_init(&ring->lock);
	init_waitqueue_head(&ring->waitq);

	sbi->log = ring;
	return 0;
}

/* ── Destroy log ring buffer ───────────────────────────────────────── */

void agfs_log_destroy(struct agfs_sb_info *sbi)
{
	struct agfs_log_ring *ring = sbi->log;

	if (!ring)
		return;

	kvfree(ring->entries);
	kfree(ring);
	sbi->log = NULL;
}

/* ── Emit a log entry ──────────────────────────────────────────────── */

void agfs_log_emit(struct agfs_sb_info *sbi, u8 event, u8 perm,
		   u32 op, const char *path, u64 req_id)
{
	struct agfs_log_ring *ring = sbi->log;
	struct agfs_log_entry *e;

	if (!ring)
		return;

	spin_lock(&ring->lock);
	e = &ring->entries[ring->head];

	memset(e, 0, sizeof(*e));
	e->timestamp_ns = ktime_get_real_ns();
	e->req_id = req_id;
	e->op = op;
	e->pid = current->pid;
	e->event = event;
	e->perm = perm;
	get_task_comm(e->comm, current);
	if (path)
		strscpy(e->path, path, AGFS_PATH_MAX);

	ring->head = (ring->head + 1) % ring->size;
	if (ring->count < ring->size)
		ring->count++;

	spin_unlock(&ring->lock);
	wake_up_interruptible(&ring->waitq);
}

/* ── read: return log entries to userspace ─────────────────────────── */

static ssize_t agfs_log_read(struct file *file, char __user *buf,
			     size_t count, loff_t *ppos)
{
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);
	struct agfs_log_ring *ring = sbi->log;
	struct agfs_log_entry entry;
	unsigned int tail;
	int err;

	if (!ring)
		return -EIO;

	if (count < sizeof(entry))
		return -EINVAL;

	/* Wait for entries */
	if (file->f_flags & O_NONBLOCK) {
		spin_lock(&ring->lock);
		if (ring->count == 0) {
			spin_unlock(&ring->lock);
			return -EAGAIN;
		}
	} else {
		err = wait_event_interruptible(ring->waitq, ring->count > 0);
		if (err)
			return err;
		spin_lock(&ring->lock);
		if (ring->count == 0) {
			spin_unlock(&ring->lock);
			return -EAGAIN;
		}
	}

	/* Read oldest entry */
	tail = (ring->head + ring->size - ring->count) % ring->size;
	entry = ring->entries[tail];
	ring->count--;
	spin_unlock(&ring->lock);

	if (copy_to_user(buf, &entry, sizeof(entry)))
		return -EFAULT;

	return sizeof(entry);
}

/* ── poll ──────────────────────────────────────────────────────────── */

static __poll_t agfs_log_poll(struct file *file,
			      struct poll_table_struct *wait)
{
	struct agfs_sb_info *sbi = AGFS_SB(file_inode(file)->i_sb);
	struct agfs_log_ring *ring = sbi->log;
	__poll_t mask = 0;

	if (!ring)
		return EPOLLERR;

	poll_wait(file, &ring->waitq, wait);

	spin_lock(&ring->lock);
	if (ring->count > 0)
		mask |= EPOLLIN | EPOLLRDNORM;
	spin_unlock(&ring->lock);

	return mask;
}

/* ── File Ops ──────────────────────────────────────────────────────── */

const struct file_operations agfs_log_fops = {
	.owner	= THIS_MODULE,
	.read	= agfs_log_read,
	.poll	= agfs_log_poll,
};
