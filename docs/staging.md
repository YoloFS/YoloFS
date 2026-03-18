# Staging-Commit Layer

The staging layer intercepts all writes and redirects them to a flat inode
store (`.agfs/inodes/`). Changes are invisible to the lower filesystem
until an explicit `commit`. An `abort` discards them instantly.

## Design Invariants

Two invariants simplify the kernel-side design. Both are **enforced** in
the kernel, not merely assumed.

1. **No open file handles during checkpoint.** The checkpoint ioctl rejects
   with `-EBUSY` if any staging fds are open (`sbi->staging_fd_count > 0`).
   The CLI naturally satisfies this by taking checkpoints between `agfs exec`
   invocations, never while agent processes are running. This means
   `sbi->checkpoint_gen` cannot change while any staging fd is open, so the
   COW decision can be made once at `open()` time — the write and mmap
   paths are pure pass-throughs with zero staging logic.

2. **Directory inodes with dirents are pinned.** When the first
   dirent is added to a directory, the kernel takes an extra `igrab()`
   on its inode, preventing eviction regardless of memory pressure.
   The dirent hash table lives on the inode (not the dentry), so it
   survives dentry eviction naturally. Pins are released in bulk by
   `AGFS_IOC_RESTORE` (called by commit/abort/restore) and during
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
├── journal                      # append-only journal (all ops)
├── inodes/                      # flat inode store (inodes/1, inodes/2, ...)
│   ├── 1                        # inode: content of some file
│   ├── 2                        # inode: content of another file
│   └── ...
└── mnt/                         # mount point -- agent works here
                                 #   ioctl on this directory fd for control
```

## In-Kernel State

All staging state lives in the structures below. Nothing is shared
across mounts. The two design invariants (no open fds during checkpoint,
directory inodes pinned) keep the state minimal: the file struct carries
zero staging-specific fields; all staging truth lives in the dirent
table on directory inodes.

**Per-superblock** (`agfs_sb_info`) — one instance, lives for the mount:

| Field | Purpose |
|-------|---------|
| `inodes_dir` | Pinned path to `.agfs/inodes/` |
| `journal_file` | Open file handle to `.agfs/journal` (`O_APPEND`). The kernel only appends; it never reads the journal back. |
| `staging_sem` | rw\_semaphore serializing COW / re-COW, checkpoint, and journal writes |
| `next_ino` | Atomic counter for inode names (`1`, `2`, …) |
| `checkpoint_gen` | Atomic counter, starts at 1, bumped by each checkpoint ioctl. Compared against `dirent.checkpoint_gen` at open time to decide COW / re-COW. |
| `staging_fd_count` | Atomic counter of open staging fds (opened for write). Checkpoint ioctl rejects with `-EBUSY` when > 0. |
| `pinned_dirs` | List head tracking `igrab()`-pinned directory inodes (those with dirents) for bulk release at cache invalidation / unmount. |
| `pinned_dirs_lock` | Spinlock protecting `pinned_dirs`. |

**Per-inode** (`agfs_inode_info`) — one per cached inode:

| Field | Purpose |
|-------|---------|
| `lower_inode` | Pointer to the lower-FS inode (base file at lookup time). Not updated after COW — stale but harmless. Used only for `evict_inode` cleanup and directory permission pass-through. |
| `de_buckets` | *(directories only)* 64-bucket hash table of `agfs_dirent` entries. Lazily allocated on first dirent. This is the kernel's source of truth for staged changes. Protected by the VFS `inode->i_rwsem` (shared for reads, exclusive for writes). |
| `de_pin` | *(directories only)* Node in `sbi->pinned_dirs`. Linked on first dirent, removed at cache invalidation / unmount. |

The dirent table lives on the directory inode because it is a property
of the directory itself, not of the dentry name. Directories are never
COW'd, so their inode identity is stable for the entire staging session.
The COW generation for regular files is tracked on the dirent entry
(`checkpoint_gen`), not on the inode.

**Per-dentry** (`agfs_dentry_info`) — one per cached dentry:

| Field | Purpose |
|-------|---------|
| `lower_path` | Resolved path to the backing file — either `inodes/<ino>` or the base file. Updated in-place by COW. |

**Per-file** (`agfs_file_info`) — one per open file descriptor:

| Field | Purpose |
|-------|---------|
| `lower_file` | Open file handle to the lower file. Always points at the correct inode (COW is resolved at open time, not deferred to write). |

No staging-specific flags. Because no fd spans a checkpoint boundary
(enforced by `staging_fd_count`), the file handle established at open
time is valid for the lifetime of the fd.

**Per-dirent** (`agfs_dirent`) — one per staged child name in a directory:

| Field | Purpose |
|-------|---------|
| `ino` | Discriminant: `>0` → staged in `inodes/<ino>`, `0` → deleted, `AGFS_INO_REDIRECT` → content at `base` |
| `base` | `NULL` → not in base; `AGFS_BASE_PRESENT` (sentinel `(char *)-1`) → exists in base but not a redirect; otherwise → absolute base path for zero-copy rename (redirect). Non-NULL means the path exists in the base layer; used at journal-write time to distinguish `A` (add) from `M` (modify) records. See [Tracking `in_base`](#tracking-in_base). |
| `d_type` | File type (`DT_REG` / `DT_DIR` / `DT_LNK`) for correct readdir emission |
| `checkpoint_gen` | `sbi->checkpoint_gen` at the time this inode was created. Used at open time: if `checkpoint_gen < sbi->checkpoint_gen`, a re-COW is needed. |

## Path Resolution

Each directory inode holds a **dirent hash table** of child dirents
(64 buckets, keyed by `full_name_hash()`). Each dirent records the
current state of a child name:

```c
#define AGFS_INO_DELETED   0ULL
#define AGFS_INO_REDIRECT  ((u64)-1)

struct agfs_dirent {
    struct hlist_node node;
    u64               ino;        /* >0 = staged, 0 = deleted, (u64)-1 = redirect */
    char              *base;      /* NULL = not in base, AGFS_BASE_PRESENT = in base,
                                   * other = redirect path (also in base) */
    u64               checkpoint_gen;    /* sbi->checkpoint_gen when inode was created */
    unsigned int      name_len;
    unsigned char     d_type;     /* DT_REG / DT_DIR / DT_LNK for readdir */
    char              name[];
};
```

Discrimination is by `ino` alone:
- `ino > 0` (and `!= AGFS_INO_REDIRECT`) → staged file, symlink, or directory
  in `inodes/<ino>`. `checkpoint_gen` records when the inode was created; if
  `checkpoint_gen < sbi->checkpoint_gen`, a re-COW is needed on the next
  open-for-write.
- `ino == AGFS_INO_REDIRECT` → redirect to `base` path (zero-copy rename)
- `ino == 0` → deleted (lookup returns negative dentry)
- no entry at all → fall through to base filesystem

**`find_dirent`** — hash lookup in the parent directory's dirent table:

```
find_dirent(dir, name):
    bucket = hash(name) >> (32 - shift)
    for de in dir.de_buckets[bucket]:
        if de.name == name:  return de
    return NULL
```

`base` is always owned by the dirent entry. Readers that outlive the
VFS `i_rwsem` (e.g. rename) must duplicate it before resolving, because
writers are free to replace the string in place when publishing a new dirent.

**`add_dirent`** — upsert: update existing dirent or insert into bucket:

```
add_dirent(dir, name, de_template):
    de = find_dirent(dir, name)
    if de:
        de.ino = de_template.ino
        de.checkpoint_gen = de_template.checkpoint_gen
        if de_template.ino == 0:
            # Deleting: keep existing base (inherit from what was here)
        else:
            de.base = dup(de_template.base)   # replace owned copy
    else:
        de = alloc_dirent(name)
        de.ino = de_template.ino
        de.checkpoint_gen = de_template.checkpoint_gen
        if de_template.ino == 0:
            de.base = AGFS_BASE_PRESENT  # no prior dirent → file was only in base
        else:
            de.base = dup(de_template.base)
        bucket = hash(name) >> (32 - shift)
        dir.de_buckets[bucket].add(de)
```

A dirent is **deleted** when `ino == 0`
(i.e. `add_dirent(dir, name)` with no other arguments).

**Lookup** (`agfs_lookup`) — called by the VFS when a name is first
accessed in a directory:

```
agfs_lookup(dir, name):
    de = find_dirent(dir, name)
    if de:
        if de.ino > 0 and de.ino != AGFS_INO_REDIRECT:
            return inode for inodes/<de.ino>   # staged
        if de.ino == AGFS_INO_REDIRECT:
            return inode for base/<de.base>  # redirect
        return negative dentry  # deleted (ino == 0)
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
write. Because no fd spans a checkpoint boundary (enforced by `staging_fd_count`),
`sbi->checkpoint_gen` is stable for the lifetime of the fd, so the decision
made at open time is final. This makes `write_iter` and `mmap` pure
pass-throughs with zero staging logic.

Staging publications that involve COW or re-COW (installing a
dirent, updating the dentry's lower path, and appending the journal
record) are serialized under `staging_sem` and must succeed as a unit.
If any step fails (e.g., journal append), the operation fails and the
previous mapping remains authoritative.
Create/mkdir/symlink/unlink/rmdir/rename are already serialized by the VFS
`inode_lock(dir)` and do not need `staging_sem`.

```
agfs_open(inode, file):
    de = find_dirent(parent, name)

    if file->f_flags & (O_WRONLY | O_RDWR):
        if de and agfs_ino_is_staged(de.ino) and de.checkpoint_gen >= sbi->checkpoint_gen:
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
            // agfs_do_cow updates: dirent (ino, checkpoint_gen,
            //   sets base=AGFS_BASE_PRESENT), dentry lower_path, journal
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
`checkpoint_gen = sbi->checkpoint_gen` so the file is recognised as already staged
(preventing a spurious re-COW on next open-for-write):

```
agfs_create(dir, name, mode):
    ino = next_ino++
    create file inodes/<ino>
    old_de = find_dirent(dir, name)
    in_base = old_de ? de_in_base(old_de) : false   # inherit from deleted dirent
    add_dirent(dir, name, ino=ino,
               base=in_base ? AGFS_BASE_PRESENT : NULL,
               checkpoint_gen=sbi->checkpoint_gen)
    if in_base:
        journal(M, dir_path, name, dtype, ino)
    else:
        journal(A, dir_path, name, dtype, ino)

agfs_mkdir(dir, name, mode):
    ino = next_ino++
    create dir inodes/<ino>/
    old_de = find_dirent(dir, name)
    in_base = old_de ? de_in_base(old_de) : false
    add_dirent(dir, name, ino=ino,
               base=in_base ? AGFS_BASE_PRESENT : NULL,
               checkpoint_gen=sbi->checkpoint_gen)
    if in_base:
        journal(M, dir_path, name, dtype, ino)
    else:
        journal(A, dir_path, name, dtype, ino)

agfs_symlink(dir, name, target):
    ino = next_ino++
    create symlink inodes/<ino> -> target
    old_de = find_dirent(dir, name)
    in_base = old_de ? de_in_base(old_de) : false
    add_dirent(dir, name, ino=ino,
               base=in_base ? AGFS_BASE_PRESENT : NULL,
               checkpoint_gen=sbi->checkpoint_gen)
    if in_base:
        journal(M, dir_path, name, dtype, ino)
    else:
        journal(A, dir_path, name, dtype, ino)
```

`touch` (create + close, no write) produces an empty inode in the store —
visible in `agfs status` / `agfs diff` and cleanly discarded by abort.

## Delete / Rmdir Path

Delete adds a "deleted" dirent (`ino = 0`) and appends to the journal:

```
agfs_unlink(dir, name):
    add_dirent(dir, name)   # ino=0 = deleted
    journal(D, dir_path, name)

agfs_rmdir(dir, name):
    add_dirent(dir, name)   # ino=0 = deleted
    journal(D, dir_path, name)
```

Subsequent lookup of the name finds the dirent and returns a negative
dentry. The base file is untouched until commit.

## Rename Handling

Rename is decomposed into a delete of the old name + creation at the new
name. No file content is copied — only dirent metadata changes.
Two records are emitted: `D` for the old name, and `A`/`M`/`R` for the
new name depending on source state.

```
agfs_rename(old_parent, old_name, new_parent, new_name):
    old_de = find_dirent(old_parent, old_name)

    if old_de and old_de.ino > 0 and old_de.ino != AGFS_INO_REDIRECT:
        # File has a staged inode -- move the dirent, keep same ino.
        # Inherit base from source dirent.
        add_dirent(new_parent, new_name, ino=old_de.ino,
                     base=old_de.base,
                     checkpoint_gen=old_de.checkpoint_gen)
    elif old_de and old_de.ino == AGFS_INO_REDIRECT:
        # Already redirected (chained rename) -- follow the chain.
        add_dirent(new_parent, new_name, ino=AGFS_INO_REDIRECT,
                     base=old_de.base)
    else:
        # File only in base -- redirect without copying.
        add_dirent(new_parent, new_name, ino=AGFS_INO_REDIRECT,
                     base=abs_path(old_parent, old_name))

    # Hide the old name (ino=0 = deleted). base is inherited
    # from the old dirent (or AGFS_BASE_PRESENT if no dirent = base-only file).
    add_dirent(old_parent, old_name)   # ino=0, inherits base

    # Emit journal records: delete old + add/modify/redirect new
    journal(D, old_dir_path, old_name)
    if staged:
        dst_de = find_dirent(new_parent, new_name)
        if de_in_base(dst_de):
            journal(M, new_dir_path, new_name, dtype, ino)
        else:
            journal(A, new_dir_path, new_name, dtype, ino)
    else:
        journal(R, new_dir_path, new_name, dtype, base)
```

**Rename chains** (`mv a->b`, then `mv b->c`) work naturally: the second
rename finds the REDIRECTED dirent on `b`, follows its `base` path to
the original base file, and creates a new REDIRECTED dirent on `c`.

**Rename + recreate** (`mv a->b`, then `touch a`) works because the new
`touch a` sees the deleted dirent (with `in_base` inherited from the
rename) and creates a new staged dirent that supersedes it.

**Read after rename**: lookup of the new name finds the dirent ->
opens the base file at the redirected path (or the staged inode).
Lookup of the old name finds the DELETED dirent -> returns `-ENOENT`.

**Write after rename**: opening for write triggers COW at open time. The
base file is copied into a new inode; the dirent changes from
`base=<path>` to `ino=N`.

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
DT_LNK) stored in each dirent. The `ino` passed to `dir_emit` is
`de->ino` directly — staged inodes use their inode-store ID, redirects
use `AGFS_INO_REDIRECT` (non-zero, so entries are never silently skipped).

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

**`fsync`**: If the file is staged (`agfs_ino_is_staged(de->ino)`),
returns 0 immediately — staged inodes are ephemeral and will be committed or
discarded as a batch. For base files opened read-only, fsync is delegated to
the lower filesystem as usual.

## Journal Format

The journal is an append-only file at `.agfs/journal`. Each record is a
sequence of NUL-separated fields terminated by a newline.

```
A\0<dir>\0<name>\0<dtype>\0<ino>\n       # add    — new file (create/mkdir/symlink)
M\0<dir>\0<name>\0<dtype>\0<ino>\n       # modify — existing file (COW)
D\0<dir>\0<name>\n                        # delete
R\0<dir>\0<name>\0<dtype>\0<base>\n       # redirect (rename, source in base)
K\0<id>\0<name>\n                         # checkpoint marker
```

Each mutation type has its own record tag and carries exactly the fields
it needs:

| Tag | Fields | Meaning |
|-----|--------|---------|
| `A` | `<dir>`, `<name>`, `<dtype>`, `<ino>` | New file: content in `inodes/<ino>` |
| `M` | `<dir>`, `<name>`, `<dtype>`, `<ino>` | Existing file modified: content in `inodes/<ino>` |
| `D` | `<dir>`, `<name>` | Entry deleted |
| `R` | `<dir>`, `<name>`, `<dtype>`, `<base>` | Redirect: rename from base path (zero-copy) |
| `K` | `<id>`, `<name>` | Checkpoint marker |

`<dir>` is the parent directory path (empty string for root).
`<name>` is the entry name within that directory.
`<dtype>` is `f` (regular file), `d` (directory), or `l` (symlink).
`<ino>` is the staged inode ID (decimal).
`<base>` is the redirect source path.

The `A` vs `M` distinction encodes whether the path existed in the base
layer before the mutation. This removes the need for filesystem checks
(`base_file().exists()`) during resolution — the record is self-describing.

### Tracking `in_base`

The kernel determines `A` vs `M` at journal-write time using the
`base` pointer on `agfs_dirent`. A non-NULL `base` (either
`AGFS_BASE_PRESENT` or a redirect path string) means the path exists in
the base layer. This is checked via `agfs_de_in_base(de)` (`de->base != NULL`),
set at staging time and inherited through deletes:

| Operation | `base` value | Rationale |
|-----------|-------------|-----------|
| `agfs_create_staged` (touch/mkdir/symlink) | `NULL` | New file, not in base |
| `agfs_do_cow` (write to existing) | `AGFS_BASE_PRESENT` | COW of base file |
| Rename (redirect, source in base) | redirect path string | Source is a base file |
| Rename (staged source) | inherited from source dirent | Preserves origin info |
| `agfs_del_dirent` (with prior dirent) | inherited from prior dirent | Preserves origin info |
| `agfs_del_dirent` (no prior dirent) | `AGFS_BASE_PRESENT` | File was only in base |

When `agfs_create_staged` is called and a deleted dirent already exists
for that name (re-create after delete), the deleted dirent's `base`
pointer determines the journal tag: `M` if non-NULL, `A` if `NULL`.

**Edge cases:**

- **Delete + re-create of a base file** (`rm x && touch x`): Delete
  inherits `base!=NULL` (file was in base). Re-create sees the deleted
  dirent with `base!=NULL` → emits `M`. The resolver correctly
  produces `Modified`.

- **Delete + re-create of a staged-only file** (`touch x && rm x && touch x`
  within a session): The first create sets `base=NULL`. Delete
  inherits `base=NULL`. Re-create sees `base==NULL` → emits `A`.
  The resolver correctly produces `Added`.

- **COW + delete + re-create** (`echo hi >> existing && rm existing && touch existing`):
  COW sets `base=AGFS_BASE_PRESENT`. Delete inherits it. Re-create
  sees `base!=NULL` → emits `M`.

- **Rename + delete + re-create at source** (`mv a b && touch a`):
  The rename emits `D` for `a`. The deleted dirent for `a` inherits
  `base!=NULL` (the file was in base). `touch a` sees `base!=NULL`
  → emits `M`. If `a` had been staged-only, `base==NULL` → emits `A`.

### Checkpoint Segments

The CLI uses `resolve_segments` to display per-checkpoint deltas. Each
segment is resolved independently with a fresh `Resolver` — records
between consecutive `K` markers form one segment. This is O(N) total.

`K` records a checkpoint. The CLI can slice the journal with
`slice_records(records, at, from, to)` before passing to
`resolve_segments`, so that only the requested range of segments is
resolved and displayed.

Slicing semantics:
- `--at <name>` — isolate the single segment at that checkpoint
  (records between the previous checkpoint and the named one).
- `--from <name>` — records after that checkpoint to end.
- `--to <name>` — records from start up to and including that checkpoint.
- `--from <A> --to <B>` — records between the two checkpoints.

Written by the kernel (`kernel_write()` per mutation). Read by the CLI
for commit/abort/status/diff. Never read by the kernel.

## Staging Operations (Userspace)

Commit and abort are **userspace operations** — the kernel module only handles
I/O redirection. The `agfs` CLI reads the journal and applies or discards.

**Commit** (`agfs commit`):

1. Replay journal in order to build a resolved operation list. Each path
   is tracked through its lifetime of mutations so that intermediate
   operations collapse into their final effect:
   - Staged + Delete collapses (path never existed in base).
   - Delete + Redirect at a new path collapses to `Rename`.
   - Redirect chains collapse: `Redirect(a→b)` then `Redirect(b→c)` → `Rename(a,c)`.
   - Multiple records for the same path keep only the final ino.
2. Apply resolved changes in order: **renames first**, then adds/modifies,
   then deletes. Renames must precede adds because a rename source path
   may overlap with a new add at the same path (e.g. `mv a b` then
   `touch a`). For each change:
   - **Rename**: `rename(base/old, base/new)`.
   - **Add/Modify**: move `inodes/<ino> -> base/path` (stat inode to
     determine type: regular file -> copy/rename, symlink -> recreate,
     directory -> mkdir), creating parent dirs as needed.
   - **Delete**: `rm base/path`.
3. Clean up: remove all files under `.agfs/inodes/`, truncate `.agfs/journal`.
4. Signal kernel to reset staging state (`AGFS_IOC_RESTORE` with `entry_count=0`, `checkpoint_gen=1`).

**Abort** (`agfs abort`):

1. Count staged changes; if none, print "nothing to discard" and exit.
2. Prompt for confirmation: `Discard N staged changes? [y/N]`.
3. Remove all files under `.agfs/inodes/` and truncate `.agfs/journal`.
4. Signal kernel to reset staging state (`AGFS_IOC_RESTORE` with `entry_count=0`, `checkpoint_gen=1`).

**Status** (`agfs status`):

1. Optionally slice journal to a range (`--at`, `--from`, `--to`).
2. Resolve into segments grouped by checkpoint boundaries.
3. Display one-line summaries under checkpoint headers (and any trailing
   unsaved changes). Print total count.

**Diff** (`agfs diff`):

1. Optionally slice journal to a range (`--at`, `--from`, `--to`).
2. Resolve into segments grouped by checkpoint boundaries.
3. For modified/added files, diff `inodes/<ino>` vs base.
   For renames, show rename metadata. For deletes, show as deleted file.
4. Output in git-style unified diff format under checkpoint headers.

## Checkpoint Mechanism

Checkpoints are named bookmarks in the journal. They enable inspecting,
diffing, and committing staged changes at specific points in time.

**Key insight**: the flat inode store already preserves all historical file
states — old inodes are never deleted (only commit/abort removes the
entire inode store). The journal records which ino was associated
with each path at each mutation. Replaying the journal up to a checkpoint
marker reconstructs the staged state at that point.

The only kernel-side change is ensuring that writes after a checkpoint create
a **new** inode instead of overwriting the current one in place.
This is the re-COW mechanism.

### Creating a Checkpoint

On mount, the kernel writes an initial checkpoint record `K\01\0(initial)\n`
to the journal, giving userspace a stable id=1 reference to the mount-time
state.

`agfs checkpoint [name]` calls `ioctl(AGFS_IOC_CHECKPOINT)`. The kernel:

1. Returns `-ENOTSUP` if `staging` is disabled (checkpoints require staging).
2. Takes `staging_sem` write lock.
3. If `staging_fd_count > 0`, releases sem and returns `-EBUSY`.
4. Increments `sbi->checkpoint_gen` (atomic counter).
5. Appends `K\0<id>\0<name>\n` to the journal.
6. Releases `staging_sem`.
7. Returns the checkpoint ID to userspace.

No dirent tables change. No caches invalidated. The write lock on
`staging_sem` ensures no open-for-write is mid-flight when the generation
is bumped (open-for-write holds at least a read lock while incrementing
`staging_fd_count`).

The name defaults to `"after <cmd>"` when auto-checkpointing via `agfs exec`
(e.g., `"after make build"`), or a human-readable timestamp like
`chk-20260315-043807` when run via `agfs checkpoint` with no argument. Names need
not be unique; checkpoints can also be addressed by their numeric ID. When
looking up by name, `--at` and `--from` match the latest one.

### Re-COW on First Open-for-Write After Checkpoint

The COW check is per-dirent: `dirent.checkpoint_gen` records the
`sbi->checkpoint_gen` at which the current inode was created.
`sbi->checkpoint_gen` starts at 1. Newly created files set
`checkpoint_gen = sbi->checkpoint_gen` at creation time, so they are already
up-to-date and skip the COW check. Base files that have no dirent
(or have `ino == 0`) naturally trigger COW on the first
open-for-write.

At open time, the COW check in `agfs_open` (see [Open / Read / Write
Path](#open--read--write-path)) handles both base→staged COW and
staged→staged re-COW: if the dirent is missing, has no ino, or has a
stale `checkpoint_gen`, a fresh inode is created.

`agfs_do_cow` copies from the dentry's current `lower_path` — which is
the base file before any COW, or the current staged inode after one.
The same function handles both cases; no separate re-COW path.
`agfs_do_cow` also updates `dirent.checkpoint_gen` after a successful COW.

Because no fd spans a checkpoint boundary (enforced by `staging_fd_count`),
the write and mmap paths need no COW checks — they are pure pass-throughs.

**Multiple checkpoints between opens** work naturally: if N checkpoints occur
without any open-for-write, the first open after them triggers one re-COW.
The generation counter collapses consecutive checkpoints.

### Example Journal with Checkpoints

```
K\01\0(initial)\n                                 # implicit checkpoint at mount
M\0/src\0main.rs\0f\01\n                          # COW: main.rs -> ino 1
A\0/src\0lib.rs\0f\02\n                           # create lib.rs -> ino 2
K\02\0after make build\n                           # checkpoint 2
M\0/src\0main.rs\0f\03\n                          # re-COW: main.rs -> ino 3
D\0/src\0lib.rs\n                                  # delete lib.rs
A\0/src\0new.rs\0f\04\n                           # create new.rs
K\03\0after make test\n                            # checkpoint 3
M\0/src\0new.rs\0f\05\n                           # re-COW: new.rs -> ino 5
```

State at each point:

| Checkpoint           | main.rs | lib.rs    | new.rs |
|-------------------|---------|-----------|--------|
| (initial)          | --      | --        | --     |
| "after make build" | ino 1   | ino 2     | --     |
| "after make test"  | ino 3   | (deleted) | ino 4  |
| current            | ino 3   | (deleted) | ino 5  |

### Checkpoint-Aware CLI Operations

All checkpoint query flags (`--at`, `--from`, `--to`) work by slicing
the journal records before resolving into segments. This means the
output always preserves checkpoint boundaries within the requested range.

**`agfs status`** / **`agfs diff`** support:
- `--at <name|id>` — show the single segment at that checkpoint.
- `--from <name|id>` — show changes from that checkpoint to end of journal.
- `--to <name|id>` — show changes from start of journal to that checkpoint.
- `--from <A> --to <B>` — show changes between two checkpoints.
- `--at` conflicts with `--from`/`--to`.

**`agfs restore <name|id>`**: Restore the mounted view to the state at the
named checkpoint. Post-checkpoint changes are discarded. The kernel's
staging state is atomically replaced with the checkpoint's resolved state
via `AGFS_IOC_RESTORE`:

1. CLI resolves journal up to the checkpoint → resolved changes.
2. CLI converts changes to dirent entries (path, ino, base, d_type).
   Entries are sorted by path — parents before children — so that
   `vfs_path_lookup` in the kernel can find staged parent directories
   when injecting child dirents.
3. `AGFS_IOC_RESTORE(checkpoint_gen=N, entries=[...])`:
   kernel wipes all dirents, shrinks dcache, injects entries, and sets
   `checkpoint_gen` to N. Done before truncating so the journal is intact
   if the ioctl fails (e.g. `EBUSY`).
4. Truncate journal after the `K` marker (`set_len` to the byte offset
   past the checkpoint record). The inode is preserved so the kernel's
   `O_APPEND` fd stays valid.
5. Orphaned inodes (from post-checkpoint mutations) remain in the inode
   store — cleaned up on the next `commit` or `abort`.

After restore, future writes trigger re-COW only if a new checkpoint is
taken (since `checkpoint_gen` is set to the restored checkpoint's
generation). Editing files without a new checkpoint reuses the
existing inodes in place.

Restoring to `(initial)` (checkpoint ID 1) produces the same mounted view
as abort — all dirents are cleared and the mount shows the clean base
state. Orphaned inodes remain in the store until the next `commit` or
`abort`.

**`agfs log`**: List all checkpoints with their names.
