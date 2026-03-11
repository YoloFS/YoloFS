# agfs Kernel Module — Implementation TODO

## Overview
Implement the agfs kernel filesystem module per DESIGN.md. agfs is a stackable
filesystem (wrapfs pattern) with two orthogonal layers:
1. **Staging-commit** — writes go to a staging directory, invisible to lower FS until commit
2. **Permission gating** — files start as `ask`, rules promote to allow/deny, ask blocks threads

Target kernel: Linux 6.8. Reference: third_party/wrapfs_nfs (wrapfs skeleton).

---

## Tasks

### 1. Build System (`Kbuild`, `Makefile`)
- Create `Kbuild` listing all object files: `agfs-y := super.o inode.o file.o dentry.o lookup.o staging.o perm.o ctl.o log.o`
- Create `Makefile` with out-of-tree build support against Linux 6.8 headers
- Verify the module compiles clean with `make`

### 2. Header (`agfs.h`)
- Define `AGFS_SUPER_MAGIC`, `AGFS_PATH_MAX` (256)
- Define `enum agfs_perm` (NONE, ASK, ALLOW, ALLOW_RW, ALLOW_RO, ALLOW_RX, DENY)
- Define `struct agfs_sb_info` per §5.1 (lower_sb, base_path, storage_path, staging_dir, renames_file, staging_sem, perm_gen, pending_reqs, pending_lock, request_waitq, next_req_id, ask_timeout_s, ask_default, log ring buffer pointer, log_size)
- Define `struct agfs_inode_info` per §5.2 (lower_inode, cached_perm, perm_gen, vfs_inode)
- Define `struct agfs_dentry_info` per §5.5 (lock, lower_path, perm)
- Define `struct agfs_file_info` per §5.6 (lower_file, needs_cow, lower_vm_ops)
- Define `struct agfs_perm_request` per §5.4 (id, path, op, pid, comm, decision, done, list)
- Define `struct agfs_ctl_request` and `struct agfs_ctl_response` per §5.3
- Define `struct agfs_log_entry` per §8.1
- Define AGFS_LOG_* event constants and AGFS_OP_* constants
- Define ioctl commands: `AGFS_IOC_RULE_ADD`, `AGFS_IOC_RULE_REMOVE`, `AGFS_IOC_CACHE_INVAL`
- Define `struct agfs_ioc_rule` for ioctl rule add/remove
- Accessor macros: `AGFS_SB()`, `AGFS_I()`, `AGFS_D()`, `AGFS_F()`
- Lower path helpers: `agfs_get_lower_path()`, `agfs_set_lower_path()`, `agfs_put_lower_path()`
- Declare all extern function prototypes (super, inode, file, dentry, lookup, staging, perm, ctl, log)

### 3. Superblock & Module Init (`super.c`)
- Implement inode/dentry slab caches (`agfs_inode_cachep`, `agfs_dentry_cachep`)
- `agfs_alloc_inode()` — allocate from slab cache
- `agfs_destroy_inode()` / `agfs_free_inode()` — free inode info
- `agfs_evict_inode()` — truncate pagecache, iput lower inode
- `agfs_statfs()` — delegate to lower, replace f_type with AGFS_SUPER_MAGIC
- `agfs_put_super()` — free sb_info, path_put base/storage/staging paths
- `agfs_show_options()` — print mount options (ask_timeout, ask_default, nogating, nostaging, log_size)
- `agfs_fill_super()` — parse mount options, resolve base_path ("/"), storage_path, staging_dir; set up sb, root inode, initialize perm gating state (waitq, pending list, perm_gen), initialize log ring buffer, parse config.toml rules
- `agfs_mount()` — call `mount_nodev()` with `agfs_fill_super`
- `agfs_kill_super()` — cleanup
- `file_system_type` registration, `module_init` / `module_exit`

### 4. Inode Operations (`inode.c`)
- `agfs_create()` — create file in staging directory via `vfs_create()`
- `agfs_mkdir()` — create dir in staging via `vfs_mkdir()`
- `agfs_unlink()` — create whiteout in staging, remove staging file if exists
- `agfs_rmdir()` — create whiteout in staging, remove staging dir if exists
- `agfs_symlink()` — create symlink in staging via `vfs_symlink()`
- `agfs_rename()` — implement §3.5 rename handling (staged file: rename within staging; base file: redirect lower_path + whiteout + append to renames file)
- `agfs_permission()` — implement §4.2 cached permission check (gen counter, delegate dirs to lower, check perm for regular files)
- `agfs_setattr()` — gated for regular files; COW base→staging first, then `notify_change()` on staging
- `agfs_getattr()` — gated for regular files; `vfs_getattr()` on resolved path
- `agfs_listxattr()` — delegate to lower
- Dir inode_operations struct, file inode_operations struct, symlink inode_operations struct

### 5. File Operations (`file.c`)
- `agfs_open()` — implement §3.4: perm gating via dentry (ask path blocks), then staging redirect (O_TRUNC → create empty staging; O_WRONLY/O_RDWR → open base + needs_cow; O_RDONLY → open resolved)
- `agfs_read_iter()` — swap kiocb->ki_filp to lower, delegate, restore
- `agfs_write_iter()` — if needs_cow, do full base→staging copy first; then swap kiocb and delegate
- `agfs_mmap()` — delegate to lower, save vm_ops
- `agfs_fsync()` — delegate to lower file
- `agfs_release()` — fput lower file, free file_info
- `agfs_llseek()` — delegate to lower
- `agfs_dir_read()` / `agfs_readdir()` — delegate iterate_dir to lower
- File operations structs: `agfs_main_fops`, `agfs_dir_fops`

### 6. Dentry Operations (`dentry.c`)
- `agfs_d_revalidate()` — delegate to lower if lower has revalidate; check staging epoch
- `agfs_d_release()` — path_put lower_path, free dentry_info
- Dentry cache init/destroy helpers
- `agfs_new_dentry_private_data()` — allocate + init spinlock + set perm=NONE
- Dentry ops structs

### 7. Lookup & Interposition (`lookup.c`)
- `agfs_lookup()` — resolve: check staging dir first (whiteout → negative dentry), check redirected lower_path, then check base; call `lookup_one_len()` on resolved lower dir, interpose
- `agfs_iget()` — `iget5_locked()` with test/set callbacks, set lower_inode, set i_op/i_fop based on mode, cache permission
- `agfs_interpose()` — get inode, d_instantiate
- Helper: `agfs_inode_test()` / `agfs_inode_set()` for iget5

### 8. Staging Helpers (`staging.c`)
- `agfs_staging_path()` — given a relative path, construct the staging path
- `agfs_base_path()` — given a relative path, construct the base path
- `agfs_resolve_lower()` — implement §3.3 path resolution (staging → redirected dentry → base → ENOENT)
- `agfs_staging_has()` — check if a file exists in staging dir
- `agfs_is_whiteout()` — check if a path is a whiteout (char dev 0/0)
- `agfs_create_whiteout()` — `mknod()` with S_IFCHR, major/minor 0/0
- `agfs_do_cow()` — full copy base→staging via `vfs_copy_file_range()`, create parent dirs
- `agfs_create_staging_parents()` — mkdir -p for staging subdirs
- `agfs_dentry_relpath()` — get path relative to mount root from dentry

### 9. Permission Gating (`perm.c`)
- `agfs_resolve_perm()` — walk up dentry chain per §4.2 until finding non-NONE perm
- `agfs_cache_perm()` — cache resolved perm on inode with gen counter
- `agfs_check_perm()` — check perm against file flags (O_RDONLY/O_WRONLY/O_RDWR), return -EACCES if denied
- `agfs_ask_userspace()` — allocate perm_request, enqueue on pending_reqs, wake request_waitq, wait_event_interruptible with timeout, return decision
- `agfs_perm_request_alloc()` / `agfs_perm_request_free()`

### 10. Control File (`ioctl.c`)
- `agfs_ctl_open()` — no-op
- `agfs_ctl_read()` — dequeue oldest pending request, `copy_to_user()` as `agfs_ctl_request`; block if empty (or EAGAIN for O_NONBLOCK)
- `agfs_ctl_write()` — `copy_from_user()` as `agfs_ctl_response`, find request by id, set decision, `complete()`
- `agfs_ctl_poll()` — return POLLIN when pending_reqs non-empty
- `agfs_ctl_ioctl()` — handle AGFS_IOC_RULE_ADD (resolve path→dentry, set perm, pin, bump perm_gen), AGFS_IOC_RULE_REMOVE (set NONE, unpin, bump perm_gen), AGFS_IOC_CACHE_INVAL (bump perm_gen, invalidate staging caches)
- `agfs_ctl_fops` struct
- `agfs_ctl_init()` / `agfs_ctl_cleanup()` — called from super.c

### 11. Log File (`log.c`)
- Ring buffer: `struct agfs_log_ring` with `agfs_log_entry` array, head/tail, spinlock
- `agfs_log_init()` — allocate ring buffer (default 1024 entries)
- `agfs_log_destroy()` — free ring buffer
- `agfs_log_emit()` — add entry to ring buffer (called from perm.c, file.c, ioctl.c)
- `agfs_log_read()` — dequeue entries, `copy_to_user()`; block if empty (EAGAIN for O_NONBLOCK)
- `agfs_log_poll()` — POLLIN when entries available
- `agfs_log_fops` struct

### 12. Build & Test
- Run `make` against Linux 6.8 headers
- Fix any compilation errors
- Ensure clean build with no warnings
