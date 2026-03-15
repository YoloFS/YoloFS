# Staging-Commit Layer

The staging layer intercepts all writes and redirects them to a flat blob
store. Changes are invisible to the lower filesystem until an explicit
`commit`. An `abort` discards them instantly.

## Concepts

| Term                  | Meaning |
| --------------------- | ------- |
| **base**              | Always `/` — the entire root filesystem, read-only from AgFS's perspective until commit. |
| **staging directory** | `.agfs/staging/` — a flat blob store. Each entry is identified by a numeric ID (`staging/1`, `staging/2`, ...). Files and symlinks are stored as blobs; directories created by `mkdir` are empty directories (children live in their own blobs). No mirrored directory tree. |
| **override table**    | Per-directory in-memory hash table of overrides. Records which children are added, modified, deleted, or renamed. This is the kernel's source of truth. |
| **journal**           | `.agfs/journal` — append-only log of all mutations. Written by the kernel, read by the CLI for commit/abort/status/diff. The kernel never reads it back. |
| **mount point**       | `.agfs/mnt/` — the agent's view of the filesystem. Shows the merged base + staged changes with permission gating applied. |
| **commit**            | CLI reads the journal and applies all operations to the base filesystem. |
| **abort**             | CLI deletes journal + staging directory. O(1). |

## Credential Override for Staging

The staging directory is created during mount (typically by root) and is owned
by root. When non-root user processes trigger staging operations through AgFS
(create, mkdir, COW, write, etc.), the VFS permission checks on the staging
directory would fail because the user lacks write permission on root-owned
directories.

To solve this, AgFS saves the mount-time credentials (`current_cred()`) in
`agfs_sb_info` during `agfs_fill_super` and uses `override_creds()` /
`revert_creds()` to temporarily assume them when performing staging directory
operations. This is the standard pattern used by OverlayFS and other stackable
filesystems. The actual permission model for user access is enforced
separately by the AgFS permission gating layer (see [permissions.md](permissions.md)),
not by Unix mode bits on the staging directory.

## Storage Layout

```
agfs.toml                       # config file in CWD (mount options + rules)
.agfs/                          # created by `agfs` in CWD
├── journal                      # append-only mutation log (all ops)
├── staging/                     # flat blob store (staging/1, staging/2, ...)
│   ├── 1                        # blob: content of some file
│   ├── 2                        # blob: content of another file
│   └── ...
└── mnt/                         # mount point -- agent works here
                                 #   ioctl on this directory fd for control
```

## Path Resolution

Each directory dentry holds an **override hash table** of child overrides
(64 buckets, keyed by `full_name_hash()`). Each override records the
current state of a child name:

```c
struct agfs_override {
    struct hlist_node node;
    u64               staging_id;  /* >0 = content/dir in staging/<id> */
    char              *base_path;  /* non-NULL = content at this base path */
    unsigned int      name_len;
    char              name[];
};
```

Interpretation:
- `staging_id > 0` -> file, symlink, or directory in `staging/<id>`
- `base_path != NULL` -> file with content at this mirrored absolute base path
  (same namespace used in the journal)
- all zero/NULL -> deleted (lookup returns negative dentry)
- no entry at all -> fall through to base filesystem

**`find_override`** — hash lookup in the parent directory's override table:

```
find_override(dir, name):
    bucket = hash(name) >> (32 - shift)
    for ovr in dir.ovr_buckets[bucket]:
        if ovr.name == name:  return ovr
    return NULL
```

`base_path` is always owned by the override entry. Readers must snapshot or
duplicate it while holding the directory spinlock before resolving it, because
writers are free to replace the string in place when publishing a new override.

**`add_override`** — upsert: update existing override or insert into bucket:

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
        bucket = hash(name) >> (32 - shift)
        dir.ovr_buckets[bucket].add(ovr)
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

**Readdir** merges the override table with the base directory:

```
agfs_readdir(dir):
    for ovr in dir.ovr_buckets[*]:
        if not ovr.is_deleted:  dir_emit(ovr.name)
    for entry in base_readdir(dir):
        if not find_override(dir, entry.name):  dir_emit(entry.name)
```

The override table is the kernel's in-memory source of truth. The journal
persists it on disk for the CLI. The kernel never reads the journal back.

## Open / Read / Write Path

The backing file (staging blob or base file) is determined at **lookup**
time via the override table. `open()` receives a dentry already pointing at
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
    //   inode->snapshot_gen == 0  -> base file, needs base->staging COW
    //   inode->snapshot_gen < sbi -> staging blob is stale, needs re-COW
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

1. `O_TRUNC` (most common: `>`, editors, code generators) -> allocate
   staging blob directly, no copy.
2. `O_RDONLY` -> open the resolved lower file (staging blob or base).
3. `O_RDWR`/`O_WRONLY` without truncate (rare: `sed -i`, `dd`, append) ->
   copy base->staging blob on first write.

## Create / Mkdir / Symlink Path

All creation operations allocate a staging blob, add an override, and set
`inode->snapshot_gen = sbi->snapshot_gen` so the file is recognised as
already staged (preventing a spurious re-COW on first write):

```
agfs_create(dir, name, mode):
    id = next_staging_id++
    create file staging/<id>
    add_override(dir, name, staging_id=id)
    inode->snapshot_gen = sbi->snapshot_gen
    journal(A, abs_path, id)

agfs_mkdir(dir, name, mode):
    id = next_staging_id++
    create dir staging/<id>/
    add_override(dir, name, staging_id=id)
    inode->snapshot_gen = sbi->snapshot_gen
    journal(A, abs_path, id)

agfs_symlink(dir, name, target):
    id = next_staging_id++
    create symlink staging/<id> -> target
    add_override(dir, name, staging_id=id)
    inode->snapshot_gen = sbi->snapshot_gen
    journal(A, abs_path, id)
```

`touch` (create + close, no write) produces an empty blob in staging —
visible in `agfs status` / `agfs diff` and cleanly discarded by abort.

## Delete / Rmdir Path

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

## Rename Handling

Rename is decomposed into a delete of the old name + creation at the new
name. No file content is copied — only override metadata changes.

```
agfs_rename(old_parent, old_name, new_parent, new_name):
    old_ovr = find_override(old_parent, old_name)

    if old_ovr and old_ovr.staging_id:
        # File is in a staging blob -- move the override, keep same blob.
        add_override(new_parent, new_name, staging_id=old_ovr.staging_id)
    elif old_ovr and old_ovr.base_path:
        # Already redirected (chained rename) -- follow the chain.
        add_override(new_parent, new_name, base_path=old_ovr.base_path)
    else:
        # File only in base -- redirect without copying.
        add_override(new_parent, new_name,
                     base_path=abs_base_path(old_parent, old_name))

    # Hide the old name (all fields zero/NULL = deleted).
    add_override(old_parent, old_name)
    journal(R, old_abs_path, new_abs_path)
```

**Rename chains** (`mv a->b`, then `mv b->c`) work naturally: the second
rename finds the REDIRECTED override on `b`, follows its `base_path` to
the original base file, and creates a new REDIRECTED override on `c`.

**Rename + recreate** (`mv a->b`, then `touch a`) works because the new
`touch a` adds an ADDED override that supersedes the DELETED override.

**Read after rename**: lookup of the new name finds the override ->
opens the base file at the redirected path (or the staging blob).
Lookup of the old name finds the DELETED override -> returns `-ENOENT`.

**Write after rename**: triggers lazy COW as usual. The base file is
copied into a new staging blob; the override changes from
`base_path=...` to `staging_id=N`.

Commit and abort handling for renames is covered in
[Staging Operations](#staging-operations-userspace).

## Readdir (Merged Directory Listing)

`readdir` (`iterate_shared`) presents a merged view: overrides first,
then base entries that aren't overridden.

```
agfs_readdir(dir, ctx):
    # 1. Emit non-deleted overrides.
    for ovr in dir.ovr_buckets[*]:
        if not ovr.is_deleted:
            dir_emit(ctx, ovr.name)

    # 2. Emit base entries not overridden by overrides.
    for entry in base_readdir(dir):
        if not find_override(dir, entry.name):
            dir_emit(ctx, entry.name)
```

The merged list is built fresh on every `readdir` call — no caching.
This ensures creates, deletes, and renames between `getdents64` calls
are always visible. Override hash tables are small, so the cost is negligible.

## setattr / getattr / fsync

**`setattr`** (e.g., `chmod`, `chown`, `truncate`): If the file is still in
the base, a COW is triggered first — the base file is copied into a staging
blob. Then the attribute change is applied to the staging blob.

**`getattr`** (e.g., `stat`): Stats from the resolved path — the staging
blob if the file has been modified, otherwise the base file.

**`fsync`**: If the file is in staging (`inode->snapshot_gen > 0`), returns 0
immediately — staging files are ephemeral and will be committed or discarded
as a batch. For base files opened read-only, fsync is delegated to the lower
filesystem as usual.

## Journal Format

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

## Staging Operations (Userspace)

Commit and abort are **userspace operations** — the kernel module only handles
I/O redirection. The `agfs` CLI reads the journal and applies or discards.

**Commit** (`agfs commit`):

1. Replay journal in order to build a resolved operation list. Each path
   is tracked through its lifetime of mutations so that intermediate
   operations collapse into their final effect:
   - `A(x) -> R(x,y)` collapses to `Add(y)` (staging file, no base rename).
   - `R(a,b) -> R(b,c)` collapses to `Rename(a,c)`.
   - `A(x) -> D(x)` cancels out (path never existed in base).
   - `R(a,b) -> A(a)` produces `Rename(a,b) + Add(a)`.
2. Apply resolved changes sequentially. For each change:
   - **Rename**: `rename(base/old, base/new)`.
   - **Delete**: `rm base/path`.
   - **Add/Modify**: move `staging/<id> -> base/path` (stat blob to
     determine type: regular file -> copy/rename, symlink -> recreate,
     directory -> mkdir), creating parent dirs as needed.
3. Clean up: remove journal + staging directory.
4. Signal kernel to invalidate caches (`AGFS_IOC_CACHE_INVAL`).

Since step 1 resolves all cross-dependencies, no particular ordering
between renames, deletes, and adds is required — each resolved
operation targets a distinct path.

**Abort** (`agfs abort`):

1. Count staged changes; if none, print "nothing to discard" and exit.
2. Prompt for confirmation: `Discard N staged changes? [y/N]`.
3. `rm -rf .agfs/staging/` and `rm .agfs/journal`.
4. Signal kernel to invalidate caches.

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

## Snapshot Mechanism

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

### Creating a Snapshot

On mount, the kernel writes an initial snapshot record `S\01\0(initial)\n`
to the journal, giving userspace a stable id=1 reference to the mount-time
state.

`agfs snapshot [name]` calls `ioctl(AGFS_IOC_SNAPSHOT)`. The kernel:

1. Returns `-ENOTSUP` if `staging` is disabled (snapshots require staging).
2. Increments `sbi->snapshot_gen` (atomic counter).
3. Appends `S\0<id>\0<name>\n` to the journal.
4. Returns the snapshot ID to userspace.

No override tables change. No caches invalidated. Existing file handles
continue working — the re-COW check triggers lazily on the next write.

The name defaults to `"after <cmd>"` when auto-snapshotting via `agfs exec`
(e.g., `"after make build"`), or a human-readable timestamp like
`snap-20260315-043807` when run via `agfs snapshot` with no argument. Names need
not be unique; snapshots can also be addressed by their numeric ID. When
looking up by name, `--at` and `--from` match the latest one.

### Re-COW on First Write After Snapshot

The COW check is purely per-inode: `inode->snapshot_gen` records the
`sbi->snapshot_gen` at which the current staging blob was created.
`sbi->snapshot_gen` starts at 1.  Newly created files set
`inode->snapshot_gen = sbi->snapshot_gen` at creation time, so
they are already up-to-date and skip the COW check.  Base files that
have never been staged have `inode->snapshot_gen == 0`, which naturally
triggers COW on first write.

On write, a single unified check handles both base->staging COW and
staging->staging re-COW:

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

### Example Journal with Snapshots

```
S\01\0(initial)\n                 # implicit snapshot at mount time
A\0/src/main.rs\01\n          # COW: main.rs -> blob 1
A\0/src/lib.rs\02\n           # create lib.rs -> blob 2
S\02\0after make build\n       # snapshot 2: "after make build"
A\0/src/main.rs\03\n          # re-COW: main.rs -> blob 3 (blob 1 preserved)
D\0/src/lib.rs\n              # delete lib.rs
A\0/src/new.rs\04\n           # create new.rs
S\03\0after make test\n        # snapshot 3: "after make test"
A\0/src/new.rs\05\n           # re-COW: new.rs -> blob 5 (blob 4 preserved)
```

State at each point:

| Snapshot           | main.rs | lib.rs    | new.rs |
|-------------------|---------|-----------|--------|
| (initial)          | --      | --        | --     |
| "after make build" | blob 1  | blob 2    | --     |
| "after make test"  | blob 3  | (deleted) | blob 4 |
| current            | blob 3  | (deleted) | blob 5 |

### Snapshot-Aware CLI Operations

**`agfs status --at <name|id>`**: Resolve journal up to the named snapshot.

**`agfs diff --from <name|id>`**: Diff changes since the named snapshot
(resolve at snapshot vs resolve at current, then diff the two states).

**`agfs commit --at <name|id>`**: Commit only changes up to the named
snapshot. Thanks to re-COW, post-snapshot blobs are independent copies —
committing pre-snapshot changes does not affect them:

1. Resolve journal up to the snapshot -> resolved changes.
2. Apply those changes to base (same as full commit).
3. Rewrite the journal atomically: write remaining post-snapshot records
   to a temporary file, fsync, then rename over the journal. The kernel's
   old journal fd (O_APPEND) continues appending to the unlinked old
   file harmlessly; `AGFS_IOC_CACHE_INVAL` in step 4 reopens it.
4. `AGFS_IOC_CACHE_INVAL`.

Orphaned staging blobs (referenced only by committed pre-snapshot records)
are left in place — they are cleaned up on the next full `commit` or
`abort`, which removes the entire staging directory.

**`agfs log`**: List all snapshots with their names and the
number of changes since the previous snapshot.
