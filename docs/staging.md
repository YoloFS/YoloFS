# Staging-Commit Layer

The staging layer intercepts all writes and redirects them to a flat inode
store (`.agfs/inodes/`). Changes are invisible to the lower filesystem
until an explicit `commit`. An `abort` discards them instantly.

## Design Invariants

Two invariants simplify the kernel-side design. Both are **enforced** in
the kernel, not merely assumed.

1. **No open file handles during snapshot.** The snapshot ioctl rejects
   with `-EBUSY` if any staging fds are open (`sbi->staging_fd_count > 0`).
   The CLI naturally satisfies this by taking snapshots between `agfs exec`
   invocations, never while agent processes are running. This means
   `sbi->snapshot_gen` cannot change while any staging fd is open, so the
   COW decision can be made once at `open()` time — the write and mmap
   paths are pure pass-throughs with zero staging logic.

2. **Directory inodes with dirents are pinned.** When the first
   dirent is added to a directory, the kernel takes an extra `igrab()`
   on its inode, preventing eviction regardless of memory pressure.
   The dirent hash table lives on the inode (not the dentry), so it
   survives dentry eviction naturally. Pins are released in bulk by
   `AGFS_IOC_CACHE_INVAL` (called by commit/abort) and during
   `kill_sb` (unmount). Directories are never COW'd, so their inode
   identity (keyed by `lower_inode` in `iget5_locked`) is stable for
   the entire staging session.

## Concepts

| Term                  | Meaning |
| --------------------- | ------- |
| **base**              | Always `/` — the entire root filesystem, read-only from AgFS's perspective until commit. |
| **inode store**       | `.agfs/inodes/` — a flat store of inodes. Each entry is identified by a numeric ino (`inodes/1`, `inodes/2`, ...). Regular files and symlinks are stored as inodes; directories created by `mkdir` are empty directory inodes (children live in their own entries). No mirrored directory tree. |
| **dirent table**    | Per-directory-inode in-memory hash table of dirents. Records which children are added, modified, deleted, or renamed. This is the kernel's source of truth. |
| **journal**           | `.agfs/journal` — append-only log of all mutations. Written by the kernel, read by the CLI for commit/abort/status/diff. The kernel never reads it back. |
| **mount point**       | `.agfs/mnt/` — the agent's view of the filesystem. Shows the merged base + staged changes with permission gating applied. |
| **commit**            | CLI reads the journal and applies all operations to the base filesystem. |
| **abort**             | CLI deletes journal + inode store. O(1). |

## Inode Store Ownership

The inode store directory (`.agfs/inodes/`) and journal file (`.agfs/journal`)
are created by the CLI during `agfs mount` and are owned by the calling user.
This means all kernel-side VFS operations on the inode store (create, mkdir,
open, lookup) run under the user's own credentials — no credential override
is needed. The AgFS permission gating layer controls access through the mount
point separately (see [permissions.md](permissions.md)).

## Storage Layout

```
agfs.toml                       # config file in CWD (mount options + rules)
.agfs/                          # created by `agfs` in CWD
├── journal                      # append-only mutation log (all ops)
├── inodes/                      # flat inode store (inodes/1, inodes/2, ...)
│   ├── 1                        # inode: content of some file
│   ├── 2                        # inode: content of another file
│   └── ...
└── mnt/                         # mount point -- agent works here
                                 #   ioctl on this directory fd for control
```

## In-Kernel State

All staging state lives in the structures below. Nothing is shared
across mounts. The two design invariants (no open fds during snapshot,
directory inodes pinned) keep the state minimal: the file struct carries
zero staging-specific fields; all staging truth lives in the dirent
table on directory inodes.

**Per-superblock** (`agfs_sb_info`) — one instance, lives for the mount:

| Field | Purpose |
|-------|---------|
| `inodes_dir` | Pinned path to `.agfs/inodes/` |
| `journal_file` | Open file handle to `.agfs/journal` (`O_APPEND`). The kernel only appends; it never reads the journal back. |
| `staging_sem` | rw\_semaphore serializing COW / re-COW, snapshot, and journal writes |
| `next_ino` | Atomic counter for inode names (`1`, `2`, …) |
| `snapshot_gen` | Atomic counter, starts at 1, bumped by each snapshot ioctl. Compared against `dirent.snapshot_gen` at open time to decide COW / re-COW. |
| `staging_fd_count` | Atomic counter of open staging fds (opened for write). Snapshot ioctl rejects with `-EBUSY` when > 0. |
| `pinned_dirs` | List head tracking `igrab()`-pinned directory inodes (those with dirents) for bulk release at cache invalidation / unmount. |
| `pinned_dirs_lock` | Spinlock protecting `pinned_dirs`. |

**Per-inode** (`agfs_inode_info`) — one per cached inode:

| Field | Purpose |
|-------|---------|
| `lower_inode` | Pointer to the lower-FS inode (base file at lookup time). Not updated after COW — stale but harmless. Used only for `evict_inode` cleanup and directory permission pass-through. |
| `de_buckets` | *(directories only)* 64-bucket hash table of `agfs_dirent` entries. Lazily allocated on first dirent. This is the kernel's source of truth for staged changes. |
| `de_lock` | *(directories only)* Spinlock protecting `de_buckets`. |
| `de_pin` | *(directories only)* Node in `sbi->pinned_dirs`. Linked on first dirent, removed at cache invalidation / unmount. |

The dirent table lives on the directory inode because it is a property
of the directory itself, not of the dentry name. Directories are never
COW'd, so their inode identity is stable for the entire staging session.
The COW generation for regular files is tracked on the dirent entry
(`snapshot_gen`), not on the inode.

**Per-dentry** (`agfs_dentry_info`) — one per cached dentry:

| Field | Purpose |
|-------|---------|
| `lower_path` | Resolved path to the backing file — either `inodes/<ino>` or the base file. Updated in-place by COW. |

**Per-file** (`agfs_file_info`) — one per open file descriptor:

| Field | Purpose |
|-------|---------|
| `lower_file` | Open file handle to the lower file. Always points at the correct inode (COW is resolved at open time, not deferred to write). |

No staging-specific flags. Because no fd spans a snapshot boundary
(enforced by `staging_fd_count`), the file handle established at open
time is valid for the lifetime of the fd.

**Per-dirent** (`agfs_dirent`) — one per staged child name in a directory:

| Field | Purpose |
|-------|---------|
| `ino` | `>0` → content lives in `inodes/<ino>` |
| `base_path` | Non-NULL → redirected to this absolute base path (zero-copy rename) |
| `d_type` | File type (`DT_REG` / `DT_DIR` / `DT_LNK`) for correct readdir emission |
| `snapshot_gen` | `sbi->snapshot_gen` at the time this inode was created. Used at open time: if `snapshot_gen < sbi->snapshot_gen`, a re-COW is needed. |
| All zero/NULL | Entry is deleted (lookup returns negative dentry) |

## Path Resolution

Each directory inode holds a **dirent hash table** of child dirents
(64 buckets, keyed by `full_name_hash()`). Each dirent records the
current state of a child name:

```c
struct agfs_dirent {
    struct hlist_node node;
    u64               ino;        /* >0 = content/dir in inodes/<ino> */
    char              *base_path; /* non-NULL = content at this base path */
    u64               snapshot_gen;    /* sbi->snapshot_gen when inode was created */
    unsigned int      name_len;
    unsigned char     d_type;     /* DT_REG / DT_DIR / DT_LNK for readdir */
    char              name[];
};
```

Interpretation:
- `ino > 0` → file, symlink, or directory in `inodes/<ino>`.
  `snapshot_gen` records when the inode was created; if `snapshot_gen < sbi->snapshot_gen`,
  a re-COW is needed on the next open-for-write.
- `base_path != NULL` → file with content at this mirrored absolute base path
  (same namespace used in the journal)
- all zero/NULL → deleted (lookup returns negative dentry)
- no entry at all → fall through to base filesystem

**`find_dirent`** — hash lookup in the parent directory's dirent table:

```
find_dirent(dir, name):
    bucket = hash(name) >> (32 - shift)
    for de in dir.de_buckets[bucket]:
        if de.name == name:  return de
    return NULL
```

`base_path` is always owned by the dirent entry. Readers must snapshot or
duplicate it while holding the inode's `de_lock` before resolving it, because
writers are free to replace the string in place when publishing a new dirent.

**`add_dirent`** — upsert: update existing dirent or insert into bucket:

```
add_dirent(dir, name, ino=0, base_path=NULL, snapshot_gen=0):
    de = find_dirent(dir, name)
    if de:
        de.ino = ino
        de.base_path = strdup(base_path)   # replace owned copy
        de.snapshot_gen = snapshot_gen
    else:
        de = alloc_dirent(name)
        de.ino = ino
        de.base_path = strdup(base_path)
        de.snapshot_gen = snapshot_gen
        bucket = hash(name) >> (32 - shift)
        dir.de_buckets[bucket].add(de)
```

A dirent is **deleted** when `ino == 0 && base_path == NULL`
(i.e. `add_dirent(dir, name)` with no other arguments).

**Lookup** (`agfs_lookup`) — called by the VFS when a name is first
accessed in a directory:

```
agfs_lookup(dir, name):
    de = find_dirent(dir, name)
    if de:
        ino, base_path = snapshot(de)   # copy under de_lock
        if ino:            return inode for inodes/<ino>
        if base_path:      return inode for base/<path>
        return negative dentry  # deleted
    return base_lookup(dir, name)   # fall through to base
```

**Readdir** merges the dirent table with the base directory:

```
agfs_readdir(dir):
    for de in dir.de_buckets[*]:
        if not de.is_deleted:  dir_emit(de.name)
    for entry in base_readdir(dir):
        if not find_dirent(dir, entry.name):  dir_emit(entry.name)
```

The dirent table is the kernel's in-memory source of truth. The journal
persists it on disk for the CLI. The kernel never reads the journal back.

## Open / Read / Write Path

The backing file (staged inode or base file) is determined at **lookup**
time via the dirent table. `open()` receives a dentry already pointing at
the right lower inode.

COW and re-COW are resolved at **open time**, not deferred to the first
write. Because no fd spans a snapshot boundary (enforced by `staging_fd_count`),
`sbi->snapshot_gen` is stable for the lifetime of the fd, so the decision
made at open time is final. This makes `write_iter` and `mmap` pure
pass-throughs with zero staging logic.

Staging publications that involve COW, re-COW, or rename (installing an
dirent, updating the dentry's lower path, and appending the journal
record) are serialized under `staging_sem` and must succeed as a unit.
If any step fails (e.g., journal append), the operation fails and the
previous mapping remains authoritative.
Create/mkdir/symlink/unlink/rmdir are already serialized by the VFS
`inode_lock(dir)` and do not need `staging_sem`.

```
agfs_open(inode, file):
    de = find_dirent(parent, name)

    if file->f_flags & (O_WRONLY | O_RDWR):
        if de and de.ino and de.snapshot_gen >= sbi->snapshot_gen:
            // Inode is current — open it directly (O_TRUNC truncates in place).
            down_read(staging_sem)
            atomic_inc(staging_fd_count)
            up_read(staging_sem)
            file_info->lower_file = open(inodes/<ino>, file->f_flags)

        else:
            // Needs COW (base file, redirected, or stale inode).
            // agfs_do_cow copies from dentry's current lower_path
            // to a fresh inode. With O_TRUNC, creates an empty inode.
            down_write(staging_sem)
            // Re-check dirent under sem — a concurrent open may have
            // already COW'd this file since our check above.
            atomic_inc(staging_fd_count)
            new_file = agfs_do_cow(sbi, dentry, flags, O_TRUNC in flags)
            // agfs_do_cow updates: dirent (ino, snapshot_gen,
            //   clears base_path), dentry lower_path, journal
            up_write(staging_sem)
            file_info->lower_file = new_file

    else:
        // Read-only: open the lower file the dentry points to.
        file_info->lower_file = open(lower_file, O_RDONLY)

agfs_release(inode, file):
    if file was opened for write:
        atomic_dec(staging_fd_count)

agfs_write_iter(kiocb, iov_iter):
    // Pure pass-through — COW already resolved at open time.
    lower_file->f_op->write_iter(...)

agfs_mmap(file, vma):
    // Pure pass-through — COW already resolved at open time.
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

1. `O_TRUNC` (most common: `>`, editors, code generators) → if the inode is
   current, truncate in place (reuses the same ino); otherwise COW
   creates an empty inode (zero copy).
2. `O_RDONLY` → open the resolved lower file (staged inode or base).
3. `O_RDWR`/`O_WRONLY` without truncate (rare: `sed -i`, `dd`, append) →
   COW at open if base file or stale inode; otherwise open inode directly.

## Create / Mkdir / Symlink Path

All creation operations allocate a new inode and add a dirent with
`snapshot_gen = sbi->snapshot_gen` so the file is recognised as already staged
(preventing a spurious re-COW on next open-for-write):

```
agfs_create(dir, name, mode):
    ino = next_ino++
    create file inodes/<ino>
    add_dirent(dir, name, ino=ino, snapshot_gen=sbi->snapshot_gen)
    journal(A, abs_path, ino)

agfs_mkdir(dir, name, mode):
    ino = next_ino++
    create dir inodes/<ino>/
    add_dirent(dir, name, ino=ino, snapshot_gen=sbi->snapshot_gen)
    journal(A, abs_path, ino)

agfs_symlink(dir, name, target):
    ino = next_ino++
    create symlink inodes/<ino> -> target
    add_dirent(dir, name, ino=ino, snapshot_gen=sbi->snapshot_gen)
    journal(A, abs_path, ino)
```

`touch` (create + close, no write) produces an empty inode in the store —
visible in `agfs status` / `agfs diff` and cleanly discarded by abort.

## Delete / Rmdir Path

Delete adds a "deleted" dirent (all fields zero/NULL) and appends
to the journal:

```
agfs_unlink(dir, name):
    add_dirent(dir, name)   # all zero = deleted
    journal(D, abs_path)

agfs_rmdir(dir, name):
    add_dirent(dir, name)   # all zero = deleted
    journal(D, abs_path)
```

Subsequent lookup of the name finds the dirent and returns a negative
dentry. The base file is untouched until commit.

## Rename Handling

Rename is decomposed into a delete of the old name + creation at the new
name. No file content is copied — only dirent metadata changes.

```
agfs_rename(old_parent, old_name, new_parent, new_name):
    old_de = find_dirent(old_parent, old_name)

    if old_de and old_de.ino:
        # File has a staged inode -- move the dirent, keep same ino.
        add_dirent(new_parent, new_name, ino=old_de.ino,
                     snapshot_gen=old_de.snapshot_gen)
    elif old_de and old_de.base_path:
        # Already redirected (chained rename) -- follow the chain.
        add_dirent(new_parent, new_name, base_path=old_de.base_path)
    else:
        # File only in base -- redirect without copying.
        add_dirent(new_parent, new_name,
                     base_path=abs_base_path(old_parent, old_name))

    # Hide the old name (all fields zero/NULL = deleted).
    add_dirent(old_parent, old_name)
    journal(R, old_abs_path, new_abs_path)
```

**Rename chains** (`mv a->b`, then `mv b->c`) work naturally: the second
rename finds the REDIRECTED dirent on `b`, follows its `base_path` to
the original base file, and creates a new REDIRECTED dirent on `c`.

**Rename + recreate** (`mv a->b`, then `touch a`) works because the new
`touch a` adds an ADDED dirent that supersedes the DELETED dirent.

**Read after rename**: lookup of the new name finds the dirent ->
opens the base file at the redirected path (or the staged inode).
Lookup of the old name finds the DELETED dirent -> returns `-ENOENT`.

**Write after rename**: opening for write triggers COW at open time. The
base file is copied into a new inode; the dirent changes from
`base_path=...` to `ino=N`.

Commit and abort handling for renames is covered in
[Staging Operations](#staging-operations-userspace).

## Readdir (Merged Directory Listing)

`readdir` (`iterate_shared`) presents a merged view: dirents first,
then base entries that aren't overridden.

```
agfs_readdir(dir, ctx):
    # 1. Emit non-deleted dirents.
    for de in dir.de_buckets[*]:
        if not de.is_deleted:
            dir_emit(ctx, de.name)

    # 2. Emit base entries not overridden by dirents.
    for entry in base_readdir(dir):
        if not find_dirent(dir, entry.name):
            dir_emit(ctx, entry.name)
```

Dirent entries are emitted with the correct `d_type` (DT_REG, DT_DIR,
DT_LNK) stored in each dirent. The `ino` passed to `dir_emit` is 0
(unknown); callers that need the real inode number should `stat()` the entry.

The merged list is built fresh on every `readdir` call — no caching.
This ensures creates, deletes, and renames between `getdents64` calls
are always visible. Dirent hash tables are small, so the cost is negligible.

## setattr / getattr / fsync

**`setattr`** (e.g., `chmod`, `chown`, `truncate`): `ATTR_MODE` is always
stripped (chmod is a no-op through AgFS). When staging is active,
`ATTR_SIZE` is stripped — truncation is handled by the COW mechanism at
open time (O_TRUNC creates an empty inode), not by setattr. Remaining
attribute changes (e.g., `chown`, timestamps) propagate directly to the
lower file (staged inode if COW'd, base file otherwise). No COW is
triggered by setattr itself.

**`getattr`** (e.g., `stat`): Stats from the resolved path — the staged
inode if the file has been modified, otherwise the base file.

**`fsync`**: If the file is in staging (the dirent has `ino > 0`),
returns 0 immediately — staged inodes are ephemeral and will be committed or
discarded as a batch. For base files opened read-only, fsync is delegated to
the lower filesystem as usual.

## Journal Format

The journal is an append-only file at `.agfs/journal`. Each record is a
sequence of NUL-terminated fields, covering ALL mutations (not just
renames). Fields within a record are separated by `\0`; records are
separated by `\n` (newline after the last `\0`).

```
A\0<path>\0<ino>\n             # content/dir in inodes/<ino>
D\0<path>\n                    # deleted
R\0<old_path>\0<new_path>\n    # rename
S\0<id>\0<name>\n              # snapshot marker (id is monotonic u64, name is human label)
```

`A` covers creates, modifies, symlinks, and mkdirs. The CLI determines
the type by stat'ing `inodes/<ino>` (regular file, symlink, or directory).
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
   - `A(x) -> R(x,y)` collapses to `Add(y)` (staged inode, no base rename).
   - `R(a,b) -> R(b,c)` collapses to `Rename(a,c)`.
   - `A(x) -> D(x)` cancels out (path never existed in base).
   - `R(a,b) -> A(a)` produces `Rename(a,b) + Add(a)`.
2. Apply resolved changes sequentially. For each change:
   - **Rename**: `rename(base/old, base/new)`.
   - **Delete**: `rm base/path`.
   - **Add/Modify**: move `inodes/<ino> -> base/path` (stat inode to
     determine type: regular file -> copy/rename, symlink -> recreate,
     directory -> mkdir), creating parent dirs as needed.
3. Clean up: remove journal + inode store.
4. Signal kernel to invalidate caches and release pinned directory inodes (`AGFS_IOC_CACHE_INVAL`).

Since step 1 resolves all cross-dependencies, no particular ordering
between renames, deletes, and adds is required — each resolved
operation targets a distinct path.

**Abort** (`agfs abort`):

1. Count staged changes; if none, print "nothing to discard" and exit.
2. Prompt for confirmation: `Discard N staged changes? [y/N]`.
3. `rm -rf .agfs/inodes/` and `rm .agfs/journal`.
4. Signal kernel to invalidate caches and release pinned directory inodes.

**Status** (`agfs status`):

1. Replay journal in order (same as commit step 1) and classify:
   renames, deletes, adds, modifies. Optionally stop at a snapshot marker
   with `--at <name>`.
2. When snapshots exist, group changes under snapshot headers showing
   which changes belong to each snapshot section (and any trailing
   unsaved changes).

**Diff** (`agfs diff`):

1. Read journal. For modified/added files, diff `inodes/<ino>` vs base.
   For renames, show rename metadata (and diff if also modified).
   For deletes, show as deleted file.
2. Output in git-style unified diff format.
3. When snapshots exist, group diffs under snapshot headers.
4. With `--from <name>`, diff changes since the named snapshot
   (resolve at snapshot vs resolve at current, then diff the two states).

## Snapshot Mechanism

Snapshots are named bookmarks in the journal. They enable inspecting,
diffing, and committing staged changes at specific points in time.

**Key insight**: the flat inode store already preserves all historical file
states — old inodes are never deleted (only commit/abort removes the
entire inode store). The journal records which ino was associated
with each path at each mutation. Replaying the journal up to a snapshot
marker reconstructs the staged state at that point.

The only kernel-side change is ensuring that writes after a snapshot create
a **new** inode instead of overwriting the current one in place.
This is the re-COW mechanism.

### Creating a Snapshot

On mount, the kernel writes an initial snapshot record `S\01\0(initial)\n`
to the journal, giving userspace a stable id=1 reference to the mount-time
state.

`agfs snapshot [name]` calls `ioctl(AGFS_IOC_SNAPSHOT)`. The kernel:

1. Returns `-ENOTSUP` if `staging` is disabled (snapshots require staging).
2. Takes `staging_sem` write lock.
3. If `staging_fd_count > 0`, releases sem and returns `-EBUSY`.
4. Increments `sbi->snapshot_gen` (atomic counter).
5. Appends `S\0<id>\0<name>\n` to the journal.
6. Releases `staging_sem`.
7. Returns the snapshot ID to userspace.

No dirent tables change. No caches invalidated. The write lock on
`staging_sem` ensures no open-for-write is mid-flight when the generation
is bumped (open-for-write holds at least a read lock while incrementing
`staging_fd_count`).

The name defaults to `"after <cmd>"` when auto-snapshotting via `agfs exec`
(e.g., `"after make build"`), or a human-readable timestamp like
`snap-20260315-043807` when run via `agfs snapshot` with no argument. Names need
not be unique; snapshots can also be addressed by their numeric ID. When
looking up by name, `--at` and `--from` match the latest one.

### Re-COW on First Open-for-Write After Snapshot

The COW check is per-dirent: `dirent.snapshot_gen` records the
`sbi->snapshot_gen` at which the current inode was created.
`sbi->snapshot_gen` starts at 1. Newly created files set
`snapshot_gen = sbi->snapshot_gen` at creation time, so they are already
up-to-date and skip the COW check. Base files that have no dirent
(or have `ino == 0`) naturally trigger COW on the first
open-for-write.

At open time, the COW check in `agfs_open` (see [Open / Read / Write
Path](#open--read--write-path)) handles both base→staged COW and
staged→staged re-COW: if the dirent is missing, has no ino, or has a
stale `snapshot_gen`, a fresh inode is created.

`agfs_do_cow` copies from the dentry's current `lower_path` — which is
the base file before any COW, or the current staged inode after one.
The same function handles both cases; no separate re-COW path.
`agfs_do_cow` also updates `dirent.snapshot_gen` after a successful COW.

Because no fd spans a snapshot boundary (enforced by `staging_fd_count`),
the write and mmap paths need no COW checks — they are pure pass-throughs.

**Multiple snapshots between opens** work naturally: if N snapshots occur
without any open-for-write, the first open after them triggers one re-COW.
The generation counter collapses consecutive snapshots.

### Example Journal with Snapshots

```
S\01\0(initial)\n                 # implicit snapshot at mount time
A\0/src/main.rs\01\n          # COW: main.rs -> ino 1
A\0/src/lib.rs\02\n           # create lib.rs -> ino 2
S\02\0after make build\n       # snapshot 2: "after make build"
A\0/src/main.rs\03\n          # re-COW: main.rs -> ino 3 (ino 1 preserved)
D\0/src/lib.rs\n              # delete lib.rs
A\0/src/new.rs\04\n           # create new.rs
S\03\0after make test\n        # snapshot 3: "after make test"
A\0/src/new.rs\05\n           # re-COW: new.rs -> ino 5 (ino 4 preserved)
```

State at each point:

| Snapshot           | main.rs | lib.rs    | new.rs |
|-------------------|---------|-----------|--------|
| (initial)          | --      | --        | --     |
| "after make build" | ino 1   | ino 2     | --     |
| "after make test"  | ino 3   | (deleted) | ino 4  |
| current            | ino 3   | (deleted) | ino 5  |

### Snapshot-Aware CLI Operations

**`agfs status --at <name|id>`**: Resolve journal up to the named snapshot.

**`agfs diff --from <name|id>`**: Diff changes since the named snapshot
(resolve at snapshot vs resolve at current, then diff the two states).

**`agfs commit --at <name|id>`**: Commit only changes up to the named
snapshot. Thanks to re-COW, post-snapshot inodes are independent copies —
committing pre-snapshot changes does not affect them:

1. Resolve journal up to the snapshot -> resolved changes.
2. Apply those changes to base (same as full commit).
3. Rewrite the journal atomically: write remaining post-snapshot records
   to a temporary file, fsync, then rename over the journal. The kernel's
   old journal fd (O_APPEND) continues appending to the unlinked old
   file harmlessly; `AGFS_IOC_CACHE_INVAL` in step 4 reopens it.
4. `AGFS_IOC_CACHE_INVAL` (releases pinned directory inodes, invalidates caches, reopens journal).

Orphaned inodes (referenced only by committed pre-snapshot records)
are left in place — they are cleaned up on the next full `commit` or
`abort`, which removes the entire inode store.

**`agfs log`**: List all snapshots with their names and the
number of changes since the previous snapshot.
