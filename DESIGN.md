# agfs — Agentic Filesystem

A Linux kernel stackable filesystem that provides **staging-commit semantics**
and **progressive permission gating** for AI-agent sandboxing.

---

## 1. Overview

agfs stacks on top of any lower filesystem (ext4, xfs, NFS, …) using VFS
interposition (the wrapfs pattern). It adds two orthogonal capabilities:


| Capability            | Summary                                                                                                                                                                                                                                                                        |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **Staging-commit**    | Every write goes to a staging layer. Changes are invisible to the lower FS until an explicit `commit`. An `abort` discards them instantly. |
| **Permission gating** | Every file starts in the `ask` state. A rule engine promotes matching paths to `allow-rw`, `allow-ro`, or `deny`. When a thread touches an `ask` file, the thread is put to sleep; a userspace daemon receives the request and writes back a decision that wakes the thread. |


Design goals:

- **In-kernel, zero-copy data path** — no FUSE overhead, no context switches
for allowed operations.
- **Unprivileged mounting** via user namespaces (same as current agfs).
- **Composable** — staging and permission gating are independent layers;
either can be disabled at mount time.

---

## 2. Architecture

```
 ┌──────────────────────────────────────────────────┐
 │                   User Process                    │
 │              (AI agent / shell / …)               │
 └────────────────────┬─────────────────────────────┘
                      │ VFS syscall
 ┌────────────────────▼─────────────────────────────┐
 │                    agfs                           │
 │       ┌─────────────┐    ┌──────────────┐        │
 │       │ Perm Gating │ →  │   Staging    │        │
 │       │   Layer     │    │    Layer     │        │
 │       └─────────────┘    └──────────────┘        │
 └────────────────────┬─────────────────────────────┘
                      │ vfs_*() on lower FS
 ┌────────────────────▼─────────────────────────────┐
 │              Lower filesystem (ext4 …)            │
 └──────────────────────────────────────────────────┘

 ┌──────────────────────────────────────────────────┐
 │  ioctl on any agfs directory fd (.agfs/mnt)      │
 │    ← AGFS_IOC_GET_REQUEST:  dequeue perm request   │
 │    → AGFS_IOC_PUT_RESPONSE: post decision           │
 │    → AGFS_IOC_RULE_ADD/REMOVE: manage rules      │
 │    → AGFS_IOC_CACHE_INVAL: invalidate caches     │
 │    → AGFS_IOC_SNAPSHOT: create snapshot           │
 └──────────────────────────────────────────────────┘
```

The two layers execute in order for every VFS operation:

1. **Perm Gating Layer** — resolves the effective permission for the file.
   Only applies to **regular files** — directories always pass through
   (controlled by standard Unix permissions on the lower FS).
   If `ask`, sleeps the calling thread. If `deny`, returns `-EACCES`.
   If `allow-*`, falls through.
2. **Staging Layer** — routes reads to the staging blob if the file has been
   modified, otherwise to the base. Ensures writes go to staging blobs.
   Uses per-directory override lists for deletions and renames.

All I/O is ultimately delegated to the lower filesystem via the standard
wrapfs pattern (`kiocb` swapping, `vfs_*()` calls).

---

## 3. Staging-Commit Mechanism

### 3.1 Concepts


| Term                  | Meaning |
| --------------------- | ------- |
| **base**              | Always `/` — the entire root filesystem, read-only from agfs's perspective until commit. |
| **staging directory** | `.agfs/staging/` — a flat blob store. Each entry is identified by a numeric ID (`staging/1`, `staging/2`, …). Files and symlinks are stored as blobs; directories created by `mkdir` are empty directories (children live in their own blobs). No mirrored directory tree. |
| **override list**     | Per-directory in-memory list of overrides. Records which children are added, modified, deleted, or renamed. This is the kernel's source of truth. |
| **journal**           | `.agfs/journal` — append-only log of all mutations. Written by the kernel, read by the CLI for commit/abort/status/diff. The kernel never reads it back. |
| **mount point**       | `.agfs/mnt/` — the agent's view of the filesystem. Shows the merged base + staged changes with permission gating applied. |
| **commit**            | CLI reads the journal and applies all operations to the base filesystem. |
| **abort**             | CLI deletes journal + staging directory. O(1). |


### 3.2 Credential Override for Staging

The staging directory is created during mount (typically by root) and is owned
by root. When non-root user processes trigger staging operations through agfs
(create, mkdir, COW, write, etc.), the VFS permission checks on the staging
directory would fail because the user lacks write permission on root-owned
directories.

To solve this, agfs saves the mount-time credentials (`current_cred()`) in
`agfs_sb_info` during `agfs_fill_super` and uses `override_creds()` /
`revert_creds()` to temporarily assume them when performing staging directory
operations. This is the standard pattern used by overlayfs and other stackable
filesystems. The actual permission model for user access is enforced
separately by the agfs permission gating layer (§4), not by Unix mode bits on
the staging directory.

### 3.3 Storage Layout

```
agfs.toml                       # config file in CWD (mount options + rules)
.agfs/                          # created by `agfs` in CWD
├── journal                      # append-only mutation log (all ops)
├── staging/                     # flat blob store (staging/1, staging/2, ...)
│   ├── 1                        # blob: content of some file
│   ├── 2                        # blob: content of another file
│   └── ...
└── mnt/                         # mount point — agent works here
                                 #   ioctl on this directory fd for control
```

### 3.4 Path Resolution

Each directory dentry holds an **override list** of child overrides. Each
override records the current state of a child name:

```c
struct agfs_override {
    struct list_head  list;
    u64               staging_id;  /* >0 = content/dir in staging/<id> */
    char              *base_path;  /* non-NULL = content at this base path */
    unsigned int      name_len;
    char              name[];
};
```

Interpretation:
- `staging_id > 0` → file, symlink, or directory in `staging/<id>`
- `base_path != NULL` → file with content at this mirrored absolute base path
  (same namespace used in the journal)
- all zero/NULL → deleted (lookup returns negative dentry)
- no entry at all → fall through to base filesystem

**`find_override`** — linear scan of the parent directory's override list:

```
find_override(dir, name):
    for ovr in dir.overrides:
        if ovr.name == name:  return ovr
    return NULL
```

`base_path` is always owned by the override entry. Readers must snapshot or
duplicate it while holding the directory spinlock before resolving it, because
writers are free to replace the string in place when publishing a new override.

**`add_override`** — upsert: update existing override or append new one:

```
add_override(dir, name, staging_id=0, base_path=NULL):
    ovr = find_override(dir, name)
    if ovr:
        ovr.staging_id = staging_id
        ovr.base_path = strdup(base_path)   # replace owned copy
    else:
        ovr = alloc_override(name)
        ovr.staging_id = staging_id
        ovr.base_path = strdup(base_path)
        dir.overrides.append(ovr)
```

An override is **deleted** when `staging_id == 0 && base_path == NULL`
(i.e. `add_override(dir, name)` with no other arguments).

**Lookup** (`agfs_lookup`) — called by the VFS when a name is first
accessed in a directory:

```
agfs_lookup(dir, name):
    ovr = find_override(dir, name)
    if ovr:
        sid, base_path = snapshot(ovr)   # copy under dir lock
        if sid:            return inode for staging/<id>
        if base_path:      return inode for base/<path>
        return negative dentry  # deleted
    return base_lookup(dir, name)   # fall through to base
```

**Readdir** merges the override list with the base directory:

```
agfs_readdir(dir):
    for ovr in dir.overrides:
        if not ovr.is_deleted:  dir_emit(ovr.name)
    for entry in base_readdir(dir):
        if not find_override(dir, entry.name):  dir_emit(entry.name)
```

The override list is the kernel's in-memory source of truth. The journal
persists it on disk for the CLI. The kernel never reads the journal back.

### 3.5 Open / Read / Write Path

The backing file (staging blob or base file) is determined at **lookup**
time via the override list. `open()` receives a dentry already pointing at
the right lower inode.

Staging publications that involve COW, re-COW, truncate-open, or rename
(installing an override, updating the dentry's lower path, and appending the
journal record) are serialized under `staging_sem` and must succeed as a unit.
Create/mkdir/symlink/unlink/rmdir are already serialized by the VFS
`inode_lock(dir)` and do not need `staging_sem`. If the journal append fails,
the write/open fails and the previous mapping remains authoritative.

```
agfs_open(inode, file):
    if file->f_flags & O_TRUNC:
        // Truncating write: allocate new staging blob, publish atomically.
        id = next_staging_id++
        file_info->lower_file = create_and_open(staging/<id>)
        down_write(staging_sem)
        add_override(parent, name, staging_id=id)
        journal(A, path, id)
        swap dentry lower path to staging/<id>
        inode->snapshot_gen = sbi->snapshot_gen
        up_write(staging_sem)

    elif file->f_flags & (O_WRONLY | O_RDWR):
        ovr = find_override(parent, name)
        if ovr and ovr.staging_id:
            // Already in staging from a prior write.
            file_info->lower_file = open(staging/<id>, file->f_flags)
        else:
            // Base file. Open read-only; first write triggers COW.
            file_info->lower_file = open(base_file, O_RDONLY)
    else:
        // Read-only: open the lower file the dentry points to.
        file_info->lower_file = open(lower_file, O_RDONLY)

agfs_write_iter(kiocb, iov_iter):
    // Unified COW / re-COW. The check is purely per-inode:
    //   inode->snapshot_gen == 0  → base file, needs base→staging COW
    //   inode->snapshot_gen < sbi → staging blob is stale, needs re-COW
    // agfs_do_cow copies from the dentry's current lower_path
    // (base or staging blob) to a fresh blob.
    if inode->snapshot_gen < sbi->snapshot_gen:
        down_write(staging_sem)
        if inode->snapshot_gen < sbi->snapshot_gen:
            new_file = agfs_do_cow(sbi, dentry, flags)
            fput(file_info->lower_file)
            file_info->lower_file = new_file
            // inode->snapshot_gen updated inside agfs_do_cow
        up_write(staging_sem)

    // Write to staging blob.
    lower_file->f_op->write_iter(...)

agfs_mmap(file, vma):
    // Unified COW / re-COW for writable shared mappings.
    if inode->snapshot_gen < sbi->snapshot_gen and
       (vma->vm_flags & (VM_WRITE | VM_SHARED)):
        down_write(staging_sem)
        if inode->snapshot_gen < sbi->snapshot_gen:
            // agfs_do_cow copies from dentry's lower_path (base or blob)
            ...
            // inode->snapshot_gen updated inside agfs_do_cow
        up_write(staging_sem)

    lower_file = file_info->lower_file
    vma->vm_file = lower_file
    ret = lower_file->f_op->mmap(lower_file, vma)
    if ret:
        vma->vm_file = file   // restore on error
    else:
        fput(file)             // balance do_mmap's get_file
    return ret
```

**Three cases, in order of likelihood for agent workloads:**

1. `O_TRUNC` (most common: `>`, editors, code generators) → allocate
   staging blob directly, no copy.
2. `O_RDONLY` → open the resolved lower file (staging blob or base).
3. `O_RDWR`/`O_WRONLY` without truncate (rare: `sed -i`, `dd`, append) →
   copy base→staging blob on first write.

### 3.6 Create / Mkdir / Symlink Path

All creation operations allocate a staging blob and add an override:

```
agfs_create(dir, name, mode):
    id = next_staging_id++
    create file staging/<id>
    add_override(dir, name, staging_id=id)
    journal(A, abs_path, id)

agfs_mkdir(dir, name, mode):
    id = next_staging_id++
    create dir staging/<id>/
    add_override(dir, name, staging_id=id)
    journal(A, abs_path, id)

agfs_symlink(dir, name, target):
    id = next_staging_id++
    create symlink staging/<id> → target
    add_override(dir, name, staging_id=id)
    journal(A, abs_path, id)
```

`touch` (create + close, no write) produces an empty blob in staging —
visible in `agfs status` / `agfs diff` and cleanly discarded by abort.

### 3.6b Delete / Rmdir Path

Delete adds a "deleted" override (all fields zero/NULL) and appends
to the journal:

```
agfs_unlink(dir, name):
    add_override(dir, name)   # all zero = deleted
    journal(D, abs_path)

agfs_rmdir(dir, name):
    add_override(dir, name)   # all zero = deleted
    journal(D, abs_path)
```

Subsequent lookup of the name finds the override and returns a negative
dentry. The base file is untouched until commit.

### 3.7 Rename Handling

Rename is decomposed into a delete of the old name + creation at the new
name. No file content is copied — only override metadata changes.

```
agfs_rename(old_parent, old_name, new_parent, new_name):
    old_ovr = find_override(old_parent, old_name)

    if old_ovr and old_ovr.staging_id:
        # File is in a staging blob — move the override, keep same blob.
        add_override(new_parent, new_name, staging_id=old_ovr.staging_id)
    elif old_ovr and old_ovr.base_path:
        # Already redirected (chained rename) — follow the chain.
        add_override(new_parent, new_name, base_path=old_ovr.base_path)
    else:
        # File only in base — redirect without copying.
        add_override(new_parent, new_name,
                     base_path=abs_base_path(old_parent, old_name))

    # Hide the old name (all fields zero/NULL = deleted).
    add_override(old_parent, old_name)
    journal(R, old_abs_path, new_abs_path)
```

**Rename chains** (`mv a→b`, then `mv b→c`) work naturally: the second
rename finds the REDIRECTED override on `b`, follows its `base_path` to
the original base file, and creates a new REDIRECTED override on `c`.

**Rename + recreate** (`mv a→b`, then `touch a`) works because the new
`touch a` adds an ADDED override that supersedes the DELETED override.

**Read after rename**: lookup of the new name finds the override →
opens the base file at the redirected path (or the staging blob).
Lookup of the old name finds the DELETED override → returns `-ENOENT`.

**Write after rename**: triggers lazy COW as usual. The base file is
copied into a new staging blob; the override changes from
`base_path=...` to `staging_id=N`.

Commit and abort handling is covered in §3.9.

### 3.8 Readdir (Merged Directory Listing)

`readdir` (`iterate_shared`) presents a merged view: overrides first,
then base entries that aren't overridden.

```
agfs_readdir(dir, ctx):
    # 1. Emit non-deleted overrides.
    for ovr in dir.overrides:
        if not ovr.is_deleted:
            dir_emit(ctx, ovr.name)

    # 2. Emit base entries not overridden by overrides.
    for entry in base_readdir(dir):
        if not find_override(dir, entry.name):
            dir_emit(ctx, entry.name)
```

The merged list is built fresh on every `readdir` call — no caching.
This ensures creates, deletes, and renames between `getdents64` calls
are always visible. Override lists are small, so the cost is negligible.

### 3.9 Journal Format

The journal is an append-only file at `.agfs/journal`. Each record is a
sequence of NUL-terminated fields, covering ALL mutations (not just
renames). Fields within a record are separated by `\0`; records are
separated by `\n` (newline after the last `\0`).

```
A\0<path>\0<id>\n          # content/dir in staging/<id>
D\0<path>\n                # deleted
R\0<old_path>\0<new_path>\n   # rename
S\0<id>\0<name>\n          # snapshot marker (id is monotonic u64, name is human label)
```

`A` covers creates, modifies, symlinks, and mkdirs. The CLI determines
the type by stat'ing `staging/<id>` (regular file, symlink, or directory).
`D` records a deletion of a file or directory.
`R` records a rename from `old_path` to `new_path`.
`S` records a snapshot. The CLI resolves the journal up to a given `S`
marker to reconstruct the staged state at that point in time.

Written by the kernel (`kernel_write()` per mutation). Read by the CLI
for commit/abort/status/diff. Never read by the kernel.

### 3.10 Staging Operations (Userspace)

Commit and abort are **userspace operations** — the kernel module only handles
I/O redirection. The `agfs` CLI reads the journal and applies or discards.

**Commit** (`agfs commit`):

1. Replay journal in order to build a resolved operation list. Each path
   is tracked through its lifetime of mutations so that intermediate
   operations collapse into their final effect:
   - `A(x) → R(x,y)` collapses to `Add(y)` (staging file, no base rename).
   - `R(a,b) → R(b,c)` collapses to `Rename(a,c)`.
   - `A(x) → D(x)` cancels out (path never existed in base).
   - `R(a,b) → A(a)` produces `Rename(a,b) + Add(a)`.
2. Apply resolved changes sequentially. For each change:
   - **Rename**: `rename(base/old, base/new)`.
   - **Delete**: `rm base/path`.
   - **Add/Modify**: move `staging/<id> → base/path` (stat blob to
     determine type: regular file → copy/rename, symlink → recreate,
     directory → mkdir), creating parent dirs as needed.
3. Clean up: remove journal + staging directory.
4. Signal kernel to invalidate caches (`AGFS_IOC_CACHE_INVAL`).

Since step 1 resolves all cross-dependencies, no particular ordering
between renames, deletes, and adds is required — each resolved
operation targets a distinct path.

**Abort** (`agfs abort`):

1. `rm -rf .agfs/staging/` and `rm .agfs/journal`.
2. Signal kernel to invalidate caches.

**Status** (`agfs status`):

1. Replay journal in order (same as commit step 1) and classify:
   renames, deletes, adds, modifies. Optionally stop at a snapshot marker
   with `--at <name>`.
2. When snapshots exist, group changes under snapshot headers showing
   which changes belong to each snapshot section (and any trailing
   unsaved changes).

**Diff** (`agfs diff`):

1. Read journal. For modified/added files, diff `staging/<id>` vs base.
   For renames, show rename metadata (and diff if also modified).
   For deletes, show as deleted file.
2. Output in git-style unified diff format.
3. When snapshots exist, group diffs under snapshot headers.
4. With `--from <name>`, diff changes since the named snapshot
   (resolve at snapshot vs resolve at current, then diff the two states).

### 3.11 Snapshot Mechanism

Snapshots are named bookmarks in the journal. They enable inspecting,
diffing, and committing staged changes at specific points in time.

**Key insight**: the flat blob store already preserves all historical file
states — old staging blobs are never deleted (only commit/abort removes the
entire staging directory). The journal records which blob ID was associated
with each path at each mutation. Replaying the journal up to a snapshot
marker reconstructs the staged state at that point.

The only kernel-side change is ensuring that writes after a snapshot create
a **new** staging blob instead of overwriting the current one in place.
This is the re-COW mechanism.

#### 3.11.1 Creating a Snapshot

`agfs snapshot [name]` calls `ioctl(AGFS_IOC_SNAPSHOT)`. The kernel:

1. Returns `-ENOTSUP` if `staging` is disabled (snapshots require staging).
2. Increments `sbi->snapshot_gen` (atomic counter).
3. Appends `S\0<id>\0<name>\n` to the journal.
4. Returns the snapshot ID to userspace.

No override lists change. No caches invalidated. Existing file handles
continue working — the re-COW check triggers lazily on the next write.

The name defaults to the executed command (if run via `agfs exec --snapshot`)
or a timestamp (if run via `agfs snapshot` with no argument). Names need
not be unique; when multiple snapshots share a name, `--at` and `--from`
match the latest one.

#### 3.11.2 Re-COW on First Write After Snapshot

The COW check is purely per-inode: `inode->snapshot_gen` records the
`sbi->snapshot_gen` at which the current staging blob was created.
`sbi->snapshot_gen` starts at 1, so `inode->snapshot_gen == 0` (no COW
yet) naturally triggers COW on first write.

On write, a single unified check handles both base→staging COW and
staging→staging re-COW:

    if inode->snapshot_gen < sbi->snapshot_gen:
        agfs_do_cow(sbi, dentry, flags)  // source = dentry's lower_path

`agfs_do_cow` copies from the dentry's current `lower_path` — which is
the base file before any COW, or the current staging blob after one.
The same function handles both cases; no separate re-COW path.
`agfs_do_cow` also updates `inode->snapshot_gen` after a successful COW.

Same check in `agfs_mmap` for writable shared mappings.

`O_TRUNC` already allocates a fresh blob every time, so the old blob is
naturally preserved. `inode->snapshot_gen` is set to `sbi->snapshot_gen`.

**Invariant**: snapshots must be taken when no staging file handles are
open (the CLI enforces this by taking snapshots between exec
invocations). This avoids the need for per-fd generation tracking —
there are no long-lived handles that span a snapshot boundary.

**Multiple snapshots between writes** work naturally: if N snapshots occur
without any write, the first write after them triggers one re-COW. The
generation counter collapses consecutive snapshots.

#### 3.11.3 Example Journal with Snapshots

```
A\0/src/main.rs\01\n          # COW: main.rs → blob 1
A\0/src/lib.rs\02\n           # create lib.rs → blob 2
S\01\0make build\n             # snapshot 1: "make build"
A\0/src/main.rs\03\n          # re-COW: main.rs → blob 3 (blob 1 preserved)
D\0/src/lib.rs\n              # delete lib.rs
A\0/src/new.rs\04\n           # create new.rs
S\02\0make test\n              # snapshot 2: "make test"
A\0/src/new.rs\05\n           # re-COW: new.rs → blob 5 (blob 4 preserved)
```

State at each point:

| Snapshot     | main.rs | lib.rs    | new.rs |
|-------------|---------|-----------|--------|
| "make build" | blob 1  | blob 2    | —      |
| "make test"  | blob 3  | (deleted) | blob 4 |
| current      | blob 3  | (deleted) | blob 5 |

#### 3.11.4 Snapshot-Aware CLI Operations

**`agfs status --at <name>`**: Resolve journal up to the named snapshot.

**`agfs diff --from <name>`**: Diff changes since the named snapshot
(resolve at snapshot vs resolve at current, then diff the two states).

**`agfs commit --at <name>`**: Commit only changes up to the named
snapshot. Thanks to re-COW, post-snapshot blobs are independent copies —
committing pre-snapshot changes does not affect them:

1. Resolve journal up to the snapshot → resolved changes.
2. Apply those changes to base (same as full commit).
3. Rewrite the journal atomically: write remaining post-snapshot records
   to a temporary file, fsync, then rename over the journal. The kernel's
   old journal fd (O_APPEND) continues appending to the unlinked old
   file harmlessly; `AGFS_IOC_CACHE_INVAL` in step 4 reopens it.
4. `AGFS_IOC_CACHE_INVAL`.

Orphaned staging blobs (referenced only by committed pre-snapshot records)
are left in place — they are cleaned up on the next full `commit` or
`abort`, which removes the entire staging directory.

**`agfs snapshot list`**: List all snapshots with their names and the
number of changes since the previous snapshot.

---

## 4. Progressive Permission Mechanism

### 4.1 Permission States

```c
enum agfs_perm {
    AGFS_PERM_NONE,        // No rule on this dentry (walk up to find one).
    AGFS_PERM_ASK,         // Default. Block thread, ask userspace.
    AGFS_PERM_ALLOW,       // Read + write + execute allowed.
    AGFS_PERM_ALLOW_RW,    // Read + write. No execute.
    AGFS_PERM_ALLOW_RO,    // Read only. No write, no execute.
    AGFS_PERM_ALLOW_RX,    // Read + execute. No write.
    AGFS_PERM_DENY,        // All access returns -EACCES.
};
```

Operations passed in ask requests:

```c
enum agfs_op {
    AGFS_OP_READ  = 1,    // File opened for reading.
    AGFS_OP_WRITE = 2,    // File opened for writing (includes append/truncate).
    AGFS_OP_EXEC  = 3,    // File opened for execution.
};
```

### 4.2 Rule Engine

Rules are **attached to dentries**. Resolved permissions are **cached on
inodes** with a **generation counter** for cheap invalidation.

Two levels:
- **Dentry**: `perm` field — only set on dentries that have an explicit rule
  (`AGFS_PERM_NONE` otherwise). Rules are pinned so the dentry is never evicted.
- **Inode**: `cached_perm` + `perm_gen` — resolved permission cached during
  `lookup()` by inheriting from the nearest ancestor dentry with a rule.
  Checked in `permission()` with O(1) cost.

**Setting a rule** (`agfs rule add src allow-rw`):

1. Write the rule to `agfs.toml` (source of truth on disk):
  ```toml
   ask_timeout = 30
   ask_default = "deny"

   [rules]
   "src"        = "allow-rw"
   "/etc"       = "deny"
   "/etc/hosts" = "allow-ro"
   "/usr/bin"   = "allow-rx"
   "/opt/bin"   = "allow"
  ```

   Paths can be **absolute** (`/etc`) or **relative** to the session root:
   the directory containing `.agfs/` (equivalently, the CWD where `agfs`
   was launched). For example, `src` resolves to
   `/home/user/project/src`.
2. If a mount exists (`.agfs/mnt` is mounted), also apply live:
   `ioctl(AGFS_IOC_RULE_ADD)` → kernel resolves the normalized absolute path
   to a dentry, sets `AGFS_D(dentry)->perm`, pins the dentry, and bumps
   `perm_gen` to invalidate all cached inode perms.

If no mount exists, the rule is persisted to `agfs.toml` only. It will be
applied on the next `agfs mount`.

On mount, the CLI reads `agfs.toml` and applies all `[rules]` via ioctl.

**Changing a rule**: just set it again + bump generation.

**Removing a rule** (`agfs rule remove /foo/bar`):

1. Remove the rule from `agfs.toml`.
2. If a mount exists, also apply live:
   `ioctl(AGFS_IOC_RULE_REMOVE)` → kernel sets `AGFS_D(dentry)->perm = NONE`,
   unpins the dentry, and bumps `perm_gen`.

**Permission resolution — cached on inode, resolved lazily**:

```c
// Resolve by walking up dentry chain (only called on cache miss).
enum agfs_perm agfs_resolve_perm(struct dentry *dentry)
{
    struct dentry *cur = dentry;
    while (cur) {
        if (AGFS_D(cur)->perm != AGFS_PERM_NONE)
            return AGFS_D(cur)->perm;
        cur = cur->d_parent;
    }
    return AGFS_PERM_ASK;
}

// Called during lookup() — cache the resolved perm on the inode.
void agfs_cache_perm(struct inode *inode, struct dentry *dentry)
{
    struct agfs_inode_info *info = AGFS_I(inode);
    struct agfs_sb_info *sb = AGFS_SB(inode->i_sb);

    info->cached_perm = agfs_resolve_perm(dentry);
    info->perm_gen = atomic64_read(&sb->perm_gen);
}

// Called by permission() — O(1) in steady state.
static int agfs_permission(struct inode *inode, int mask)
{
    struct agfs_inode_info *info = AGFS_I(inode);
    struct agfs_sb_info *sb = AGFS_SB(inode->i_sb);

    if (!S_ISREG(inode->i_mode))
        return inode_permission(info->lower_inode, mask);

    // Check generation — re-resolve if stale.
    enum agfs_perm perm = info->cached_perm;
    if (info->perm_gen != atomic64_read(&sb->perm_gen)) {
        struct dentry *dentry = d_find_alias(inode);
        if (dentry) {
            perm = agfs_resolve_perm(dentry);
            info->cached_perm = perm;
            info->perm_gen = atomic64_read(&sb->perm_gen);
            dput(dentry);
        }
    }

    if (perm == AGFS_PERM_ASK)
        return 0;  // ask is handled in open(), not here

    switch (perm) {
    case AGFS_PERM_ALLOW:     return 0;
    case AGFS_PERM_ALLOW_RW:
        return (mask & MAY_EXEC) ? -EACCES : 0;
    case AGFS_PERM_ALLOW_RO:
        return (mask & (MAY_WRITE | MAY_EXEC)) ? -EACCES : 0;
    case AGFS_PERM_ALLOW_RX:
        return (mask & MAY_WRITE) ? -EACCES : 0;
    case AGFS_PERM_DENY:      return -EACCES;
    default:                  return -EACCES;
    }
}
```

The root dentry has `perm = AGFS_PERM_ASK`. In steady state (no rule
changes), `permission()` is a single generation compare + switch — O(1).
On rule change, the generation bumps and inodes re-resolve lazily on
next access.

The `ask` path is handled in `agfs_open()` where the dentry is directly
available and the thread can sleep:

```c
static int agfs_open(struct inode *inode, struct file *file)
{
    struct dentry *dentry = file->f_path.dentry;
    int err;

    if (S_ISDIR(inode->i_mode))
        goto do_open;

    enum agfs_perm perm = AGFS_I(inode)->cached_perm;

    if (perm == AGFS_PERM_ASK) {
        char buf[AGFS_PATH_MAX];
        char *relpath = dentry_path_raw(dentry, buf, AGFS_PATH_MAX);
        if (IS_ERR(relpath))
            return PTR_ERR(relpath);   // -ENAMETOOLONG if path won't fit

        unsigned int op;
        if (file->f_mode & FMODE_EXEC)
            op = AGFS_OP_EXEC;
        else if (file->f_flags & (O_WRONLY | O_RDWR | O_APPEND | O_TRUNC))
            op = AGFS_OP_WRITE;
        else
            op = AGFS_OP_READ;

        err = agfs_ask_userspace(AGFS_SB(inode->i_sb), dentry,
                                 relpath, op, &perm);
        if (err)
            return err;
    }

    err = agfs_check_perm(perm, file->f_flags);
    if (err)
        return err;

do_open:
    // ... staging redirect (lazy COW) ...
}
```

**Example**:

```bash
 agfs rule add src          allow-rw
 agfs rule add /etc         deny
 agfs rule add /etc/hosts   allow-ro
 agfs rule add /usr/bin     allow-rx
```

- `permission("src/main.rs")` → cached_perm=ALLOW_RW (from lookup) → **pass**
- `permission("etc/passwd")` → cached_perm=DENY → **-EACCES**
- `permission("etc/hosts")` → cached_perm=ALLOW_RO → **pass for read, deny write**
- `open("tmp/foo")` → cached_perm=ASK → ask daemon → **sleeps until decision**

### 4.3 The Ask Protocol

When a thread accesses a file whose effective permission is `ask`:

```
  Thread (kernel)                          Daemon (userspace)
  ──────────────                           ──────────────────
  1. agfs_check_perm() → perm == ASK
  2. Allocate agfs_perm_request {
       id, path, op, pid, comm
     }
  3. Enqueue request on sb->pending_reqs
  4. wake_up(&sb->request_waitq)
  5. wait_event_interruptible(              ioctl(GET_REQUEST) blocks
       req->done,                            until request is available
       req->decision != UNDECIDED            ↓
     )                                      dequeue request
     …thread sleeps…                         → struct agfs_ctl_request { id, path, op, ... }
                                             ↓
                                            Daemon shows prompt / applies policy
                                             ↓
                                             ioctl(PUT_RESPONSE) → struct agfs_ctl_response {
                                                         id: 42, decision: ALLOW_RW }
                                              ↓
   6. req->decision = ALLOW_RW               ioctl handler:
   7. complete(&req->done)                     find request by id
     …thread wakes…                           set decision
  8. Proceed with operation                  complete(&req->done)
     (one-time; daemon may separately
      `ioctl(RULE_ADD)` to persist)
```

Key properties:

- **Interruptible sleep**: The thread can be killed with `SIGKILL`. The
request is removed from the pending list and `-EINTR` is returned.
- **Timeout**: Configurable via mount option `ask_timeout=<seconds>`.
If the daemon doesn't respond, the default action (configurable:
`deny` or `allow-ro`) is applied.
- **Minimal response**: `agfs_ctl_response` only carries `{ id, decision }`.
  Persisting policy is always a separate `ioctl(AGFS_IOC_RULE_ADD)`.
- **One-time by default**: The decision applies to this single access only.
  Next access to the same file triggers ask again. To persist a decision,
  the daemon separately calls `ioctl(AGFS_IOC_RULE_ADD)` to install a rule
  on the dentry.

---

## 5. Data Structures

### 5.1 Superblock Info

```c
struct agfs_sb_info {
    struct super_block     *lower_sb;
    struct path             base_path;       // always "/"
    struct path             storage_path;    // .agfs/ directory
    const struct cred      *creator_cred;    // mount-time credentials (§3.2)

    // Staging
    struct path             staging_dir;       // .agfs/staging/ (flat blob store)
    struct file            *journal_file;      // .agfs/journal (opened at mount, append-only)
    struct rw_semaphore     staging_sem;       // protects staging + journal writes
    atomic64_t              next_staging_id;   // counter for staging blob IDs
    atomic64_t              snapshot_gen;      // bumped on each snapshot; triggers re-COW

    // Permission gating
    atomic64_t              perm_gen;        // bumped on rule change; invalidates inode caches
    struct list_head        pending_reqs;    // list of agfs_perm_request
    spinlock_t              pending_lock;
    wait_queue_head_t       request_waitq;   // daemon blocks here
    atomic64_t              next_req_id;
    atomic_t                has_daemon;      // 1 if a watch daemon is connected
    unsigned int            ask_timeout_s;   // seconds, 0 = infinite
    enum agfs_perm          ask_default;     // fallback on timeout
    bool                    perm;            // enable permission gating
    bool                    staging;         // enable staging area
};
```

### 5.2 Inode Info

```c
struct agfs_inode_info {
    struct inode           *lower_inode;     // passthrough target

    // Permission cache (resolved at lookup, checked at permission)
    enum agfs_perm          cached_perm;
    u64                     perm_gen;        // sb->perm_gen at time of caching

    // Staging COW tracking (§3.11.2)
    u64                     snapshot_gen;    // sbi->snapshot_gen at last COW (0 = no COW yet)

    struct inode            vfs_inode;       // embedded VFS inode
};
```

### 5.3 Control Protocol (binary, ioctl-based)

Fixed-size structs for the ioctl-based control interface on any agfs directory
fd (typically `.agfs/mnt`). No parsing — just `copy_to_user()` /
`copy_from_user()`.

```c
#define AGFS_PATH_MAX 256

// kernel → userspace: AGFS_IOC_GET_REQUEST returns one of these
struct agfs_ctl_request {
    __u64   id;
    __u32   op;                  // AGFS_OP_READ / WRITE / EXEC
    __u32   pid;
    char    comm[16];
    char    path[AGFS_PATH_MAX];
};

// userspace → kernel: AGFS_IOC_PUT_RESPONSE accepts one of these
struct agfs_ctl_response {
    __u64   id;
    __u8    decision;            // enum agfs_perm value
    __u8    _pad[7];
};

// userspace ↔ kernel: AGFS_IOC_SNAPSHOT (name in, id out)
struct agfs_ioc_snapshot {
    __u64   id;                  // out: assigned snapshot ID
    char    name[AGFS_PATH_MAX]; // in: human-readable name (NUL-terminated)
};
```

`path` is never truncated in-kernel. If the resolved mounted-view path does
not fit in `AGFS_PATH_MAX` bytes including the terminating NUL, the access
fails with `-ENAMETOOLONG` and no ask request is enqueued.

### 5.4 Pending Permission Request (internal)

```c
struct agfs_perm_request {
    struct kref             ref;            // refcounted lifetime
    u64                     id;
    char                    path[AGFS_PATH_MAX];
    enum agfs_op            op;
    pid_t                   pid;
    char                    comm[TASK_COMM_LEN];

    enum agfs_perm          decision;       // set by daemon
    struct completion        done;          // thread sleeps on this
    struct list_head         list;          // sb->pending_reqs
};
```

### 5.5 Dentry Info

```c
struct agfs_dentry_info {
    spinlock_t              lock;
    struct path             lower_path;      // resolved lower path (staging blob or base)
    enum agfs_perm          perm;            // AGFS_PERM_NONE unless this dentry has a rule
    struct list_head        overrides;       // agfs_override list (for directories)
};
```

Each directory dentry holds an override list of child overrides that
differ from the base filesystem. See §3.4 for the `agfs_override` struct.

### 5.6 File Info

```c
struct agfs_file_info {
    struct file            *lower_file;     // opened lower file (base or staging)
    const struct vm_operations_struct *lower_vm_ops;
    struct agfs_ctl_private *ctl;           // non-NULL if this fd is a ctl daemon
};
```

No per-fd snapshot state is needed. The COW check uses
`inode->snapshot_gen < sbi->snapshot_gen` (purely per-inode). The fsync
optimization uses `inode->snapshot_gen > 0`. The CLI enforces that
snapshots are only taken when no staging file handles are open (§3.11.2),
so there are no stale cross-snapshot handles to track.

### 5.7 Ctl Private (per-fd daemon state)

```c
struct agfs_ctl_private {
    struct list_head        dispatched;     // requests sent to this fd
    spinlock_t              lock;
};
```

Allocated lazily on first `AGFS_IOC_GET_REQUEST`. On fd close, any
dispatched-but-unanswered requests receive the default decision.

### 5.8 Concurrency

| Lock | Protects | Type |
|---|---|---|
| `sb->staging_sem` | Publishing staging mutations atomically (override + journal + dentry swap + `inode->snapshot_gen`) | `rw_semaphore` (write for rename/COW/truncate-open). Create/mkdir/symlink/unlink/rmdir are serialized by VFS `inode_lock(dir)` and do not need `staging_sem`. |
| `sb->pending_lock` | Pending request queue | `spinlock` |
| `dentry_info->lock` | Per-directory override list + cached lower path | `spinlock` |

**Lock ordering**: `staging_sem` → `pending_lock` → `dentry_info->lock`
---

## 6. VFS Operations Map

### 6.1 Superblock Ops


| Operation       | Behavior                                                        |
| --------------- | --------------------------------------------------------------- |
| `alloc_inode`   | Allocate `agfs_inode_info` from slab cache.                     |
| `free_inode`    | Free inode info via slab cache.                                 |
| `evict_inode`   | Clear inode, drop lower inode ref.                              |
| `statfs`        | Delegate to lower FS. Replace `f_type` with `AGFS_SUPER_MAGIC`. |
| `put_super`     | Free `agfs_sb_info`, deactivate lower super.                    |
| `show_options`  | Print mount options.                                            |


### 6.2 Inode Ops (Directory)


| Operation    | Perm check                                                   | Staging layer                                                                     | Passthrough                               |
| ------------ | ------------------------------------------------------------ | --------------------------------------------------------------------------------- | ----------------------------------------- |
| `lookup`     | —                                                            | Check override list first (deleted → ENOENT, staging_id → blob, base_path → redirect); fall back to base. | `lookup_one_len()` on base dir. |
| `create`     | — (dir perm via lower FS)                                    | Allocate staging blob, add override + journal append.                           | `vfs_create()` on staging blob. |
| `mkdir`      | — (dir perm via lower FS)                                    | Allocate staging dir, add override + journal append.                            | —                               |
| `unlink`     | — (dir perm via lower FS)                                    | Add DELETED override, journal append.                                           | —                                         |
| `rmdir`      | — (dir perm via lower FS)                                    | Add DELETED override, journal append.                                           | —                                         |
| `rename` | — (dir perm via lower FS) | See §3.7 Rename Handling. | — |
| `symlink`    | — (dir perm via lower FS)                                    | Allocate staging blob (symlink), add override + journal append.                 | `vfs_symlink()`.                          |
| `permission` | **Gating for regular files (O(1) cached); delegate to lower FS for dirs.** | —                                                                                 | `inode_permission()` on lower inode.      |
| `setattr`    | Gated (regular files only).                                  | Copy base→staging first, then setattr on staging.                                 | `notify_change()` on lower.               |
| `getattr`    | Gated (regular files only).                                  | Stat from resolved path (staging or base).                                        | `vfs_getattr()` on lower.                 |


### 6.3 File Ops


| Operation    | Behavior                                                                                                                                                                                          |
| ------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `open`       | Perm gating (via dentry). If writable and staging file exists: open staging file. If writable and no staging file: open base read-only (COW on first write). If read-only: open resolved lower file. |
| `read_iter`  | Swap `kiocb->ki_filp` to lower file, call `lower->read_iter()`.                                                                                                                                   |
| `write_iter` | If `inode->snapshot_gen < sbi->snapshot_gen`: unified COW/re-COW via `agfs_do_cow` (source = dentry's `lower_path`). Then delegate to `lower->write_iter()`. |
| `mmap`       | If `inode->snapshot_gen < sbi->snapshot_gen` and mapping is writable+shared: trigger COW/re-COW. Then delegate to lower file. |
| `fsync`      | If `inode->snapshot_gen > 0`: return 0 (staging files are ephemeral). Otherwise delegate to lower.                                                            |
| `release`    | `fput()` lower file. Free `agfs_file_info`.                                                                                                                                                       |
| `llseek`     | Delegate to lower.                                                                                                                                                                                |


### 6.4 Dentry Ops


| Operation      | Behavior                                                                                              |
| -------------- | ----------------------------------------------------------------------------------------------------- |
| `d_revalidate` | If lower FS has revalidate, delegate. Also check staging epoch (invalidate if commit/abort occurred). |
| `d_release`    | Free `agfs_dentry_info`.                                                                              |


---

## 7. Control Interface (ioctl on directory fd)

The control interface is exposed via ioctl on any agfs directory file
descriptor (typically `.agfs/mnt`). Directory fds on the agfs mount carry
both the standard directory operations (`iterate_shared`, `llseek`) and
the control/rule/ctl ioctl handler.

### 7.1 Ioctl Commands

```c
// Permission rule management
#define AGFS_IOC_RULE_ADD        _IOW('A', 10, struct agfs_ioc_rule)
#define AGFS_IOC_RULE_REMOVE     _IOW('A', 11, struct agfs_ioc_rule)
#define AGFS_IOC_CACHE_INVAL     _IO('A', 20)

// Control protocol (ask request / response)
#define AGFS_IOC_GET_REQUEST        _IOR('A', 30, struct agfs_ctl_request)
#define AGFS_IOC_PUT_RESPONSE       _IOW('A', 31, struct agfs_ctl_response)

// Snapshot
#define AGFS_IOC_SNAPSHOT           _IOWR('A', 40, struct agfs_ioc_snapshot)
```

### 7.2 Operations

| Ioctl                    | Behavior                                                                                                                                                                       |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `AGFS_IOC_GET_REQUEST`     | Dequeue the oldest pending permission request. Returns one `struct agfs_ctl_request` (fixed-size binary). Blocks if queue is empty (or returns `-EAGAIN` for `O_NONBLOCK`).     |
| `AGFS_IOC_PUT_RESPONSE`    | Submit a decision: one `struct agfs_ctl_response` (fixed-size binary). Wakes the sleeping thread.                                                                               |
| `AGFS_IOC_RULE_ADD`     | Add a permission rule to a dentry. Kernel resolves the path, sets `AGFS_D(dentry)->perm`, pins the dentry, and bumps `perm_gen`.                                                |
| `AGFS_IOC_RULE_REMOVE`  | Remove a rule from a dentry. Kernel sets `perm = NONE`, unpins the dentry, and bumps `perm_gen`.                                                                                |
| `AGFS_IOC_CACHE_INVAL`  | Bump `perm_gen`, shrink dentry/inode caches, and reopen the journal file. Called by userspace after commit/abort.                                                           |
| `AGFS_IOC_SNAPSHOT`     | Bump `snapshot_gen`, append `S` record to journal, return snapshot ID. Triggers lazy re-COW on next write to any staged file (§3.11). |

`AGFS_IOC_CACHE_INVAL` is called by userspace after commit/abort. It:
1. Bumps `perm_gen` to invalidate all cached inode permissions.
2. Calls `shrink_dcache_sb()` to drop stale dentry caches so the mount
   reflects the new base state.
3. Closes and reopens the journal file (the CLI deletes it on
   commit/abort, so the old fd is stale).

On the first `AGFS_IOC_GET_REQUEST`, a per-fd `agfs_ctl_private` is lazily
allocated to track dispatched requests. Only one daemon is allowed at a time;
a second `GET_REQUEST` from a different fd returns `EBUSY`. On fd close, any
dispatched-but-unanswered requests receive the default decision (from
`ask_default` mount option). If no daemon is connected, ask requests are
resolved immediately using `ask_default`.

---

## 8. CLI Interface

### 8.1 Commands

**Setup**

```bash
$ agfs init              # create a default agfs.toml in the current directory
```

**Full workflow** — mount, watch, exec, diff, and prompt to commit/abort in one command:

```bash
$ agfs                   # launch sh inside the sandbox
$ agfs -- make build     # run a specific command instead of sh
$ agfs --snapshot -- make build  # snapshot before exec (or set snapshot=true in agfs.toml)
```

**Session management** — manual control over each step:

```bash
$ agfs mount             # create .agfs/ layout and mount the filesystem
$ agfs exec              # chroot $SHELL into .agfs/mnt (requires existing mount)
$ agfs exec -- make build
$ agfs exec --snapshot -- make build  # snapshot before exec
$ agfs status            # show staged changes
$ agfs status --at <name> # show state at a snapshot
$ agfs diff              # git-style diff of staged vs base (rename-aware)
$ agfs diff --from <name> # diff changes since snapshot
$ agfs commit            # apply staged changes to base
$ agfs commit --at <name> # commit only changes up to a snapshot
$ agfs abort             # discard staged changes
$ agfs unmount           # tear down session
```

**Snapshots:**

```bash
$ agfs snapshot              # snapshot with timestamp as name
$ agfs snapshot "checkpoint" # snapshot with explicit name
$ agfs snapshot list         # list all snapshots
```

**Permission rules and diagnostics:**

```bash
$ agfs rule add src allow-rw
$ agfs rule remove src
$ agfs watch             # handle ask requests (daemon mode)
```

### 8.2 Options

Configured via top-level keys in `agfs.toml`:

| Option | Default | Description |
|---|---|---|
| `ask_timeout` | 0 (infinite) | Seconds before ask request times out |
| `ask_default` | `deny` | Fallback when no daemon is connected or on timeout |
| `permission` | true | Enable permission gating |
| `staging` | true | Enable staging area |
| `snapshot` | false | Auto-snapshot before each `agfs exec` invocation |

Inside the launched shell or command, agfs `chroot`s into `.agfs/mnt` so
that the mounted view becomes `/`. The working directory remains the
caller's original CWD. For example, launching from `/home/user/project`
chroots into `.agfs/mnt` and sets the working directory to
`/home/user/project` — same absolute path, but now resolved through the
agfs mount. That is why runtime examples use absolute paths like `/src` and
`/etc` even when a rule was added as the relative path `src` from the
session root. Files under that session root are typically ruled `allow-rw`;
everything else defaults to `ask`.

### TTY / terminal ownership

When agfs runs the default workflow (`agfs` with no subcommand), a
background watch thread handles interactive permission prompts by reading
from the terminal. While the child shell is running, its process group
typically becomes the terminal foreground group. Without special handling
the parent's watch thread would receive `SIGTTIN` when it tries to read
from the terminal, stopping the entire process.

To avoid this, `watch.rs` temporarily claims terminal ownership around each
permission prompt:

1. Save the current foreground process group (`tcgetpgrp`).
2. Ignore `SIGTTIN`/`SIGTTOU` so `tcsetpgrp` won't be stopped.
3. Call `tcsetpgrp` to make the watch thread's process group the
   foreground group.
4. Restore `SIGTTIN`/`SIGTTOU` to default — we are now the foreground
   group so they won't fire.
5. Print the prompt and read the user's answer from stdin.
6. Give the terminal back to the saved foreground group.

---

## 9. Source File Layout

```
agfs/
├── DESIGN.md                  # This file
├── kmod/                      # Kernel module
│   ├── Kbuild
│   ├── agfs.h
│   ├── super.c
│   ├── inode.c
│   ├── file.c
│   ├── dentry.c
│   ├── lookup.c
│   ├── staging.c
│   ├── journal.c
│   ├── perm.c
│   └── ioctl.c
├── Cargo.toml
├── Cargo.lock
├── cli/                       # Userspace CLI source (Rust)
│   ├── main.rs
│   ├── lib.rs
│   ├── config.rs              # agfs.toml management (rules, mount options)
│   ├── init.rs                # `agfs init/deinit/reinit` — kmod load/unload, teardown
│   ├── mount.rs               # mount, unmount, remount (prompts to kill blocking procs)
│   ├── exec.rs
│   ├── commit.rs
│   ├── abort.rs
│   ├── status.rs
│   ├── diff.rs
│   ├── journal.rs             # journal parsing + resolution (commit/status/diff)
│   ├── snapshot.rs            # snapshot create, list, snapshot-aware operations
│   ├── watch.rs               # permission prompt daemon (handles TTY ownership)
│   └── ioctl.rs               # binary protocol structs + ioctl helpers
└── tests/                     # Integration tests
```

---

## 10. Lifecycle Example

```
# 1. Full interactive workflow (mount → watch + run → diff → commit/abort)
$ cd /home/user/project
$ agfs
   → creates .agfs/, mounts / → .agfs/mnt, applies rules from agfs.toml,
     starts background watch daemon for permission requests, chroots into
     .agfs/mnt, spawns $SHELL with cwd preserved as the caller's original CWD
   → on shell exit: stops watch daemon, runs `agfs diff`, prompts user to
     commit, abort, or keep staged (user runs `agfs unmount` when done)

# 1b. Or use individual commands for more control:
$ agfs mount
$ agfs watch &           # start daemon in background
$ agfs exec -- make build
$ agfs diff
$ agfs commit

# 1c. Install rules via CLI from the session root (attaches perm directly
#     to dentries)
$ agfs rule add src allow-rw
$ agfs rule add /etc deny
$ agfs rule add /etc/hosts allow-ro

# 2. Agent writes to a file matching an allow-rw rule
$ echo "hello" > /src/main.rs
   → kernel: agfs_lookup("src") → explicit rule on dentry → perm=ALLOW_RW
   → kernel: agfs_lookup("main.rs") → no rule on dentry (NONE)
              → agfs_cache_perm() walks up: main.rs(NONE) → src(ALLOW_RW)
              → caches ALLOW_RW on main.rs inode
   → kernel: agfs_open() → cached_perm=ALLOW_RW, O_WRONLY → pass
   → kernel: agfs_write_iter() → lazy COW, delegate to staging file

# 3. Agent reads /etc/passwd (denied — /etc has deny rule)
$ cat /etc/passwd
   → kernel: agfs_lookup("etc") → explicit rule on dentry → perm=DENY
   → kernel: agfs_lookup("passwd") → no rule on dentry (NONE)
              → agfs_cache_perm() walks up: passwd(NONE) → etc(DENY)
              → caches DENY on passwd inode
   → kernel: agfs_open("passwd") → cached_perm=DENY → -EACCES

# 4. Agent reads /etc/hosts (explicit override → allow-ro)
$ cat /etc/hosts
   → kernel: agfs_lookup("hosts") → explicit rule on dentry → perm=ALLOW_RO
              → agfs_cache_perm() → caches ALLOW_RO on hosts inode
   → kernel: agfs_open() → cached_perm=ALLOW_RO → pass

# 5. Agent reads /tmp/secrets (no rule anywhere → walk up reaches root → ask)
$ cat /tmp/secrets
   → kernel: agfs_lookup("tmp") → no rule on dentry (NONE)
   → kernel: agfs_lookup("secrets") → no rule on dentry (NONE)
              → agfs_cache_perm() walks up: secrets(NONE) → tmp(NONE) → root(ASK)
              → caches ASK on secrets inode
   → kernel: agfs_open() → cached_perm=ASK
   → kernel: enqueue request, thread sleeps
   → daemon: ioctl(GET_REQUEST) → agfs_ctl_request { id:1, path:"/tmp/secrets", ... }
   → daemon: decision: allow-ro
   → daemon: ioctl(PUT_RESPONSE, agfs_ctl_response { id:1, decision:ALLOW_RO })
   → kernel: wake thread, apply one-shot ALLOW_RO to this open
   → kernel: open base/tmp/secrets read-only, proceed

# 6. Agent tries to write /etc/hosts (walk up finds ALLOW_RO)
$ echo x >> /etc/hosts
   → kernel: agfs_open() → ALLOW_RO, O_WRONLY → -EACCES

# 7. Commit all staged changes to the real filesystem (userspace)
$ agfs commit
   → userspace: replay journal — apply renames, deletes, copy blobs to base
   → userspace: ioctl(AGFS_IOC_CACHE_INVAL) on .agfs/mnt
   → kernel: invalidate dentry + inode caches
   → umount .agfs/mnt
```

---

## 11. Key Design Decisions

### Why dentry walk-up instead of other rule engines?

The rule engine must satisfy these design principles:

1. **Fast checks** — permission resolution must not scale with the number of
   rules. O(n) scanning per access is unacceptable.
2. **Hierarchical rules** — a single rule on a directory applies to all files
   underneath. Rules can overlap (e.g., `/etc` = deny, `/etc/hosts` = allow-ro)
   and the most specific path always wins, regardless of insertion order.
3. **Dynamic rules** — adding, changing, or removing a rule must take effect
   immediately without expensive cache invalidation.

These principles rule out most alternatives:

| Approach | Violates |
|---|---|
| Sorted array scan | #1 — O(n) per access |
| First-match glob list | #1 — O(n), #2 — order-dependent |
| Dentry-cached inheritance | #3 — rule change requires flushing children |
| Per-file hashtable | #2 — no subtree support without enumerating all files |

The **dentry tree is already a path-component trie**. Walking `d_parent` is
longest-prefix-match for free. This satisfies all three principles:

1. O(depth) — typically 3-8 pointer hops, independent of rule count.
2. Walk finds the nearest ancestor with a rule — subtrees, overlaps, and
   per-file overrides all fall out naturally from bottom-up traversal.
3. Just set `dentry->perm` — no child invalidation, immediate effect.

### Why stackable VFS instead of block-level COW?

- **Portability**: Works on any underlying filesystem (ext4, xfs, NFS, tmpfs).
- **File-level granularity**: Permission gating operates on files and
  directories, which map naturally to inodes in a stackable FS.
- **Simplicity**: No need to manage block allocation, journaling, or
  filesystem metadata. The lower FS handles all of that.

### Permission cache invalidation

- On rule add/remove: `atomic_inc(&sb->perm_gen)`. All inode caches go
  stale; next `permission()` call re-resolves lazily via `d_find_alias()` +
  walk up. O(1) invalidation.
- On `AGFS_IOC_CACHE_INVAL` (after userspace commit/abort): bumps perm_gen,
  shrinks the dentry cache, and reopens the journal file.
- On `rename`: pure renames do **not** bump `perm_gen`. The inode keeps its
  `cached_perm` until some later invalidation event (rule add/remove or
  `AGFS_IOC_CACHE_INVAL`). This is intentional: rename is treated as a path
  move, not an immediate permission re-resolution point. A file moved from
  `src` under `/etc` may therefore continue to use its pre-rename effective
  permission until the next generation bump. This trades strict
  post-rename freshness for O(1) steady-state checks.

**Limitation**: directory permissions are not gated — only regular files
are checked. Directory access is controlled by standard Unix permissions on
the lower FS. This is intentional: gating directories would require
intercepting `lookup()` and `readdir()`, adding latency to every path
traversal. For agent sandboxing, controlling file-level read/write/exec is
sufficient.

---

## 12. Comparison with overlayfs

agfs uses a fundamentally different staging model from overlayfs.

**Staging vs live union**: overlayfs is a live union filesystem — the upper
layer *is* the persistent state. There is no commit or abort. A renamed
file is copied up to upper with `RENAME_WHITEOUT` and stays there forever.
agfs treats staging as a flat blob store with in-memory override lists that
are explicitly committed or discarded via the journal.

**Copy-up**: overlayfs always does a full copy-up on first write, even for
truncating writes (`echo "x" > file` copies the entire file, then
truncates). agfs detects `O_TRUNC` and creates an empty staging blob
directly — zero copy for the most common agent write pattern.

**Rename**: overlayfs does a real `vfs_rename()` in the upper directory,
which requires copy-up. agfs does zero-copy renames by adding overrides
(DELETED on old parent, REDIRECTED on new parent). Rename chains
resolve naturally through the override list.

**Lookup**: overlayfs does two lookups per component (upper + lower) and
merges the results. agfs checks the parent's override list first, then
falls back to base — one lookup.

**Permission model**: overlayfs uses standard Unix permissions only. agfs
adds the progressive gating layer (ask/allow/deny) with the ask protocol
for interactive approval.

**On-disk format**: overlayfs requires filesystem support for whiteouts
(`RENAME_WHITEOUT`, ext4/xfs). agfs uses a flat blob store + append-only
journal, working on any lower FS.

---

## 13. Comparison with Landlock

Landlock is a Linux Security Module (LSM) for unprivileged process
sandboxing. It shares the goal of path-based access control but differs
significantly in design.

**Rule interface**: Landlock uses file descriptors to identify paths. The
userspace process opens a path with `O_PATH`, passes the fd to
`landlock_add_rule()`, and the kernel resolves it to an inode. Rules follow
the inode, not the name — immune to rename attacks. agfs uses path strings
resolved to dentries; rules are name-based and stay on the dentry.

**Rule storage**: Landlock stores rules in an rb-tree keyed by inode object
pointer, one tree per ruleset. On access, it walks up every ancestor of the
target path and does an rb-tree lookup for each — O(depth × log n). agfs
stores rules directly on dentries and caches the resolved permission on
inodes with a generation counter — O(1) in steady state.

**Overlapping rules**: Landlock is additive — rules only grant permissions.
If `/foo` has no rule and `/foo/bar` has `READ`, then `/foo/bar` is
readable but `/foo/baz` is denied. However, you **cannot** deny a child
when a parent is allowed: if `/foo` grants `READ`, then `/foo/bar` also
gets `READ` and there is no way to revoke it. agfs uses nearest-ancestor
wins: `/foo = allow-rw` + `/foo/bar = deny` works because the walk-up
finds `/foo/bar`'s rule first. Both directions (allow parent deny child,
deny parent allow child) are supported.

**Dynamic rules**: Landlock rulesets are immutable once enforced via
`landlock_restrict_self()`. You cannot add or remove rules at runtime.
agfs rules can be added, changed, or removed at any time via ioctl, with
O(1) invalidation via generation counter.

**Default policy**: Landlock is deny-by-default for "handled" access rights.
agfs is ask-by-default — unmatched paths trigger the ask protocol, which
blocks the thread until a daemon decides.

**Scope**: Landlock is per-process (attached to credentials, inherited by
children). agfs is per-mount (all processes inside the mount share the same
rules and staging area).

| Aspect | Landlock | agfs |
|---|---|---|
| Rule target | fd → inode (follows renames) | path → dentry (name-based) |
| Rule storage | rb-tree per ruleset | `perm` field on dentry |
| Access check | O(depth × log n) per ancestor | O(1) via inode cache + gen counter |
| Overlap support | Additive only (can't deny child of allowed parent) | Nearest-ancestor wins (both directions) |
| Dynamic rules | No (immutable after enforce) | Yes (add/remove/change anytime) |
| Default | Deny (handled rights) | Ask (block + prompt) |
| Scope | Per-process (cred-attached) | Per-mount |
| Staging | N/A | Full commit/abort staging layer |
