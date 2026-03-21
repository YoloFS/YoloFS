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
   `sbi->gen` cannot change while any staging fd is open, so the
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
| `gen` | Atomic counter, starts at 1, bumped by each checkpoint and restore ioctl. Compared against the gen field in the packed dirent at open time to decide COW / re-COW. |
| `staging_fd_count` | Atomic counter of open staging fds (opened for write). Checkpoint ioctl rejects with `-EBUSY` when > 0. |
| `dirty` | Boolean flag set on every data journal write (A/M/D/R/P), cleared on checkpoint or restore. Used by `AGFS_CHK_IF_CHANGED` to skip empty auto-checkpoints. |
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
(`gen`), not on the inode.

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
| `packed` | Single `u64` encoding the dirent state. Three mutually exclusive variants discriminated by bit 63 and zero: **tombstone** (`packed == 0`, always `in_base=true`), **link** (bit 63 set — kernel pointer with tag), **inode** (bit 63 clear, non-zero — staged in `inodes/<ino>`). Bits [62:61] encode `d_type` (2-bit private encoding), bit [60] encodes `in_base`. Inode variant packs `ino` (44 bits) and `gen` (16 bits). See [Packed Encoding](#packed-encoding). |

## Path Resolution

Each directory inode holds a **dirent hash table** of child dirents
(64 buckets, keyed by `full_name_hash()`). Each dirent records the
current state of a child name:

```c
struct agfs_dirent {
    struct hlist_node   node;       /* 16 bytes */
    u64                 packed;     /*  8 bytes */
    unsigned int        name_len;   /*  4 bytes */
    char                name[];     /*  flexible */
};  /* 28 bytes fixed overhead before name[] */
```

### Packed Encoding

Three mutually exclusive states discriminated by **bit 63** and zero:

- `packed == 0` → **tombstone** (deleted). Always `in_base=true`.
  A delete of an `in_base=false` entry removes the entry entirely
  (cancelled-entry removal) rather than creating a tombstone.
- `(s64)packed < 0` → **link** (bit 63 set — kernel pointer with tag).
  Zero-copy rename redirect to a base path.
- `packed != 0 && (s64)packed >= 0` → **inode** (bit 63 clear, non-zero).
  Staged content in `inodes/<ino>`.
- no entry at all → fall through to base filesystem

`d_type` (bits [62:61]) and `in_base` (bit [60]) occupy the same
position in both inode and link variants, making common accessors
branchless.

**Inode layout** (bit 63 clear, non-zero):

```
[63]     0                 (tag: inode)
[62:61]  d_type    2 bits  (private 2-bit encoding)
[60]     in_base   1 bit
[59:16]  ino      44 bits  (max ~17.6 trillion; always > 0)
[15:0]   gen      16 bits  (max 65535)
```

**Link layout** (bit 63 set):

The `kstrdup` pointer (≥ 8-byte aligned) is stored with bit 63 set as
the tag. This coincides with the kernel sign-extension bit (always 1
for kernel addresses). `d_type` and `in_base` borrow bits [62:60]
from sign-extension (safe on x86_64 — at least bits [63:60] are 1 for
kernel addresses).

```
[63]     1                    (tag — matches kernel sign extension)
[62:61]  d_type    2 bits     (borrowed from sign extension)
[60]     in_base   1 bit      (borrowed from sign extension)
[59:0]   pointer bits [59:0]  (real address bits)
```

Pointer recovery: `packed | 0x7000000000000000`
(restore sign-extension bits [62:60] = 111; bit 63 is already 1).

**d_type 2-bit encoding**: The libc `DT_*` constants need 4 bits.
We compress to 2 bits with a private encoding, converting at
encode/decode boundaries:

| 2-bit | Meaning   | Libc       |
|-------|-----------|------------|
| `00`  | regular   | `DT_REG`   |
| `01`  | directory | `DT_DIR`   |
| `10`  | symlink   | `DT_LNK`   |
| `11`  | invalid   | bug check  |

**Cancelled-entry removal**: When `add_dirent` transitions an entry to
tombstone and the existing entry has `in_base=false`, the entry is
removed from the hash table entirely (`hlist_del` + `kfree`) instead of
being kept as a tombstone. This is safe because lookup treats `!de`
(fall through to base) and `in_base=false` tombstone identically — no
base entry exists in either case. Benefits: less memory, faster
lookups/readdir, and the tombstone becomes a single zero constant (no
`in_base` variation).

**Branchless accessors**:

```c
is_tombstone = !packed
is_link      = (s64)packed < 0
is_inode     = (s64)packed > 0
d_type       = agfs_dtype_unpack((packed >> 61) & 3)   // inode + link
in_base      = (packed >> 60) & 1                       // inode + link
ino          = (packed >> 16) & 0xFFFFFFFFFFF           // inode only
gen          = (u16)packed                              // inode only
base         = packed | 0x7000000000000000              // link only
```

`in_base` is orthogonal to the variant — it tracks whether the
destination name existed in the base filesystem, inherited through
deletes and used to select journal tags (A/M for staged, R/P for
renames).

**`find_dirent`** — hash lookup in the parent directory's dirent table:

```
find_dirent(dir, name):
    bucket = hash(name) >> (32 - shift)
    for de in dir.de_buckets[bucket]:
        if de.name == name:  return de
    return NULL
```

Link base pointers are owned by the packed encoding. Readers that
outlive the VFS `i_rwsem` (e.g. rename) must duplicate the base string
before resolving, because writers are free to replace the entry.

**`add_dirent`** — upsert: update existing dirent or insert into bucket:

```
add_dirent(dir, name, de_template):
    de = find_dirent(dir, name)
    if de:
        if de_template is tombstone:
            if agfs_pde_in_base(de.packed):
                # Transitioning to tombstone, entry was in base:
                # free any link pointer, set packed = 0.
                free_link_if_needed(de.packed)
                de.packed = 0
            else:
                # Cancelled entry: in_base=false tombstone is useless.
                # Remove entirely — lookup treats !de same as
                # in_base=false tombstone.
                free_link_if_needed(de.packed)
                hlist_del(de)
                kfree(de)
        else:
            free_link_if_needed(de.packed)
            de.packed = de_template.packed
    else:
        de = alloc_dirent(name)
        if de_template is tombstone:
            de.packed = 0           # no prior dirent → file was only in base
        else:
            de.packed = de_template.packed
        bucket = hash(name) >> (32 - shift)
        dir.de_buckets[bucket].add(de)
```

A dirent is **tombstoned** when `packed == 0`
(i.e. `del_dirent(dir, name)` which passes an all-zero template).

**Lookup** (`agfs_lookup`) — called by the VFS when a name is first
accessed in a directory:

```
agfs_lookup(dir, name):
    de = find_dirent(dir, name)
    if de:
        if agfs_pde_is_inode(de.packed):
            return inode for inodes/<agfs_pde_ino(de.packed)>   # staged
        if agfs_pde_is_link(de.packed):
            return inode for base/<agfs_pde_base(de.packed)>  # link
        return negative dentry  # tombstone (packed == 0)
    return base_lookup(dir, name)   # fall through to base
```

**Readdir** merges the dirent table with the base directory:

```
agfs_readdir(dir):
    for de in dir.de_buckets[*]:
        if not agfs_pde_is_tombstone(de.packed):
            dir_emit(de.name, agfs_pde_emit_ino(de.packed), agfs_pde_d_type(de.packed))
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
`sbi->gen` is stable for the lifetime of the fd, so the decision
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
    packed = read_dirent(parent, name)   # returns u64 packed (0 if no entry)

    if file->f_flags & (O_WRONLY | O_RDWR):
        if agfs_pde_is_current(packed, sbi->gen):
            // Inode is current — open it directly (O_TRUNC truncates in place).
            down_read(staging_sem)
            atomic_inc(staging_fd_count)
            up_read(staging_sem)
            file_info->lower_file = open(inodes/<agfs_pde_ino(packed)>, file->f_flags)

        else:
            // Needs COW (base file, link, or stale inode).
            // agfs_do_cow copies from dentry's current lower_path
            // to a fresh inode. With O_TRUNC, creates an empty inode.
            down_write(staging_sem)
            // Re-check dirent under sem — a concurrent open may have
            // already COW'd this file since our check above.
            atomic_inc(staging_fd_count)
            new_file = agfs_do_cow(sbi, dentry, flags, O_TRUNC in flags)
            // agfs_do_cow updates: dirent (packed with new ino, gen,
            //   in_base=true), dentry lower_path, journal
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

agfs_fallocate(file, mode, offset, len):
    // Pass-through to lower file if supported by the lower fs.
    lower_file->f_op->fallocate(lower_file, mode, offset, len)

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
`gen = sbi->gen` so the file is recognised as already staged
(preventing a spurious re-COW on next open-for-write):

```
agfs_create(dir, name, mode):
    ino = next_ino++
    create file inodes/<ino>
    old_de = find_dirent(dir, name)
    in_base = old_de ? agfs_pde_in_base(old_de.packed) : false   # inherit from tombstone
    add_dirent(dir, name,
               packed=agfs_pde_inode(ino, sbi->gen, dt, in_base))
    if in_base:
        journal(M, path, dtype, ino)
    else:
        journal(A, path, dtype, ino)

agfs_mkdir(dir, name, mode):
    ino = next_ino++
    create dir inodes/<ino>/
    old_de = find_dirent(dir, name)
    in_base = old_de ? agfs_pde_in_base(old_de.packed) : false
    add_dirent(dir, name,
               packed=agfs_pde_inode(ino, sbi->gen, dt, in_base))
    if in_base:
        journal(M, path, dtype, ino)
    else:
        journal(A, path, dtype, ino)

agfs_symlink(dir, name, target):
    ino = next_ino++
    create symlink inodes/<ino> -> target
    old_de = find_dirent(dir, name)
    in_base = old_de ? agfs_pde_in_base(old_de.packed) : false
    add_dirent(dir, name,
               packed=agfs_pde_inode(ino, sbi->gen, dt, in_base))
    if in_base:
        journal(M, path, dtype, ino)
    else:
        journal(A, path, dtype, ino)
```

`touch` (create + close, no write) produces an empty inode in the store —
visible in `agfs status` / `agfs diff` and cleanly discarded by abort.

## Delete / Rmdir Path

Delete adds a tombstone dirent (`packed = 0`) and appends to the journal.
If the existing entry has `in_base=false`, the entry is removed entirely
(cancelled-entry removal) instead of creating a tombstone:

```
agfs_unlink(dir, name):
    del_dirent(dir, name)   # packed=0 = tombstone (or cancel if in_base=false)
    journal(D, path, dtype)

agfs_rmdir(dir, name):
    del_dirent(dir, name)   # packed=0 = tombstone (or cancel if in_base=false)
    journal(D, path, dtype)
```

Subsequent lookup of the name finds the dirent and returns a negative
dentry. The base file is untouched until commit.

## Rename Handling

Rename is decomposed into a delete of the old name + creation at the new
name. No file content is copied — only dirent metadata changes.

All renames — staged or redirect, file or directory — emit a single R or P
record. R if the destination is NOT in base; P if the destination IS in base.

```
agfs_rename(old_parent, old_name, new_parent, new_name):
    old_de = find_dirent(old_parent, old_name)
    dst_de = find_dirent(new_parent, new_name)
    dst_in_base = dst_de ? agfs_pde_in_base(dst_de.packed) : file_exists_in_base(new_name)
    dst = join(new_parent, new_name)
    src = join(old_parent, old_name)

    if old_de and agfs_pde_is_inode(old_de.packed):
        # File has a staged inode -- move the dirent, keep same ino.
        add_dirent(new_parent, new_name,
                     packed=agfs_pde_inode(agfs_pde_ino(old_de.packed),
                                          agfs_pde_gen(old_de.packed),
                                          d_type, dst_in_base))
    else:
        # File only in base or already a link -- create link to
        # the current dentry path (no chain resolution).
        add_dirent(new_parent, new_name,
                     packed=agfs_pde_link(src, d_type, dst_in_base))

    # Tombstone the old name. in_base is inherited via cancelled-entry
    # removal (in_base=false entries are removed entirely).
    del_dirent(old_parent, old_name)

    # All renames emit a single R or P record.
    if dst_in_base:
        journal(P, dst, src, dtype)
    else:
        journal(R, dst, src, dtype)
```

**Rename chains** (`mv a->b`, then `mv b->c`) work naturally: the second
rename finds the link dirent on `b`, uses its dentry path as the old
path, and creates a new link dirent on `c`. The dirent stores the
current dentry path (not the resolved base path) — each rename is
recorded as a separate journal entry and replayed in order at commit time.

**Rename + recreate** (`mv a->b`, then `touch a`) works because the new
`touch a` sees the tombstone dirent (with `in_base` inherited from the
rename) and creates a new staged dirent that supersedes it. The rename
emits `R(b, a)` (or `P(b, a)` if `b` was in base), so the journal sees
`R(b, a)` for the rename, then `A(a)` or `M(a)` for the recreate.

**Read after rename**: lookup of the new name finds the dirent ->
opens the base file at the link's base path (or the staged inode).
Lookup of the old name finds the tombstone dirent -> returns `-ENOENT`
(or no entry if cancelled).

**Write after rename**: opening for write triggers COW at open time. The
base file is copied into a new inode; the dirent changes from a link
to an inode variant.

Commit and abort handling for renames is covered in
[Staging Operations](#staging-operations-userspace).

### Rename Overwrite: R vs P Tag

When a rename overwrites an existing base file at the destination,
the kernel emits P instead of R. The P tag
tells the tree builder that the destination path existed in base. While the
moved node occupies the position, it hides the base file. If the node is
later moved away, a Tombstone is placed to keep the base file hidden.

This applies to all renames — both staged and redirect.

```
# Both a and b exist in base.
mv b a      # Journal: P(/a, /b, f)      — destination /a is in base
mv a c      # Journal: R(/c, /a, f)      — destination /c is not in base
```

## Readdir (Merged Directory Listing)

`readdir` (`iterate_shared`) presents a merged view: dirents first,
then base entries that aren't overridden.

```
agfs_readdir(dir, ctx):
    # 1. Emit non-tombstone dirents.
    for de in dir.de_buckets[*]:
        if not agfs_pde_is_tombstone(de.packed):
            dir_emit(ctx, de.name,
                     agfs_pde_emit_ino(de.packed),
                     agfs_pde_d_type(de.packed))

    # 2. Emit base entries not overridden by dirents.
    for entry in base_readdir(dir):
        if not find_dirent(dir, entry.name):
            dir_emit(ctx, entry.name)
```

Dirent entries are emitted with the correct `d_type` (DT_REG, DT_DIR,
DT_LNK) decoded from the packed field. The `ino` passed to `dir_emit`
is the real ino for inode entries and `(u64)-1` for links (preserving
the `AGFS_INO_REDIRECT` behavior so readdir output is unchanged).

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

**`fsync`**: If the file is staged (`agfs_pde_is_inode(packed)`),
returns 0 immediately — staged inodes are ephemeral and will be committed or
discarded as a batch. For base files opened read-only, fsync is delegated to
the lower filesystem as usual.

## Journal Format

The journal is an append-only file at `.agfs/journal`. Each record is a
sequence of NUL-separated fields terminated by a newline.

```
A\0<path>\0<dtype>\0<ino>\n       — Add (new path)
M\0<path>\0<dtype>\0<ino>\n       — Modify (existing path)
D\0<path>\0<dtype>\n              — Delete
R\0<dst>\0<src>\0<dtype>\n             — Rename (destination is new)
P\0<dst>\0<src>\0<dtype>\n             — Replace (destination existed in base)
K\0<gen>\0<name>\n                — Checkpoint
T\0<gen>\0<target_gen>\n          — Restore
```

Each mutation type has its own record tag and carries exactly the fields
it needs. A/M and R/P form symmetric pairs encoding whether the
destination path existed in the base layer (`in_base`):

| Tag | Fields | `in_base` | Meaning |
|-----|--------|:---------:|---------|
| `A` | `<path>`, `<dtype>`, `<ino>` | false | Staged content at new path |
| `M` | `<path>`, `<dtype>`, `<ino>` | true | Staged content replacing base file |
| `D` | `<path>`, `<dtype>` | — | Entry deleted |
| `R` | `<dst>`, `<src>`, `<dtype>` | false | Rename to new path |
| `P` | `<dst>`, `<src>`, `<dtype>` | true | Rename replacing base file |
| `K` | `<gen>`, `<name>` | — | Checkpoint marker |
| `T` | `<gen>`, `<target_gen>` | — | Restore marker |

**Gen_id invariant.** The kernel increments `sbi->gen` via
`atomic_inc_return()` on every K and T record. Gen_id values are
strictly sequential: marker\[i\] has gen_id = i + 1. The `Markers` type
relies on this for O(1) checkpoint lookup by gen_id.

`<path>` is the full overlay path (e.g. `/dir/file`).
`<src>` is the overlay path before the rename (R/P only).
`<dtype>` is `f` (regular file), `d` (directory), or `l` (symlink).
`<ino>` is the staged inode ID (decimal).

All renames — staged or redirect, file or directory — emit a single R or P
record. R if the destination is new; P if it overwrites a base path. The
tree builder handles the rest.

The A/M and R/P distinctions encode whether the path had
existing content before the mutation (`in_base`). This removes the
need for filesystem checks during resolution — each record is
self-describing.

### Tracking `in_base`

The kernel determines the journal tag at write time using `in_base`
decoded from the packed dirent (`agfs_pde_in_base(de->packed)`). This
flag means "the path had existing content at operation time" — it
applies to both base and staged content. The field is set at staging
time and inherited through deletes:

| Operation | `in_base` | Rationale |
|-----------|:---------:|-----------|
| `agfs_create_staged` (touch/mkdir/symlink) | `false` | New path, no prior content |
| `agfs_create_staged` (re-create after delete) | inherited | Inherits from tombstone dirent |
| `agfs_do_cow` (write to existing) | `true` | Path had content (base or staged) |
| Rename (any source) | `dst_in_base` | Whether destination had content |
| `agfs_del_dirent` (with prior dirent, in_base=true) | inherited | Tombstone preserves origin info |
| `agfs_del_dirent` (with prior dirent, in_base=false) | — | Entry removed entirely (cancelled) |
| `agfs_del_dirent` (no prior dirent) | `true` | Path had content (base only) |

The link base pointer in the packed encoding is used only for link
source paths. It is not used as an `in_base` indicator.

When `agfs_create_staged` is called and a tombstone dirent already exists
for that name (re-create after delete), the tombstone's `in_base`
determines the journal tag: M if true, A if false. (Tombstones always
have `in_base=true` after cancelled-entry removal, so re-creates after
a base file delete always emit M.)

**Edge cases:**

- **Delete + re-create of a base file** (`rm x && touch x`): Delete
  inherits `in_base=true` (file was in base). Re-create sees the deleted
  dirent with `in_base=true` → emits M. The tree builder correctly
  processes the `Modify` action.

- **Delete + re-create of a staged-only file** (`touch x && rm x && touch x`
  within a session): The first create sets `in_base=false`. Delete triggers
  cancelled-entry removal (entry removed entirely). Re-create finds no
  dirent, defaults `in_base=false` → emits A. The tree builder correctly
  processes the `Add` action.

- **COW + delete + re-create** (`echo hi >> existing && rm existing && touch existing`):
  COW sets `in_base=true`. Delete inherits it. Re-create
  sees `in_base=true` → emits M.

- **Rename + delete + re-create at source** (`mv a b && touch a`):
  The rename emits `R(b, a)` (or `P(b, a)` if `b` was in base). `touch a` sees the
  deleted dirent with inherited `in_base=true` (base file existed) →
  emits M. If `a` had been staged-only, delete triggers cancelled-entry
  removal, so `touch a` finds no dirent, defaults `in_base=false` → emits A.

- **Rename-overwrite + move** (`mv b a && mv a c`, both `a` and `b` in
  base): The first rename overwrites base `a`. Because `dst_in_base` is
  true, the kernel emits `P(/a, /b, f)`. The second rename emits `R(/c, /a, f)`.
  The tree builder processes P (moves `b` to `a` with `in_base=true`,
  Tombstone at `b`) then R (moves `a` to `c` with `in_base=false`,
  Tombstone at `a`). Final tree: node at `/c`, Tombstones at `/a` and `/b`.

- **Rename-overwrite + delete** (`mv b a && rm a`, both in base):
  Same mechanism — kernel emits `P(/a, /b, f)`, then `D(/a, f)`.
  The tree builder produces Tombstones at `/a` and `/b`.

- **Directory rename with staged children** (`echo x > dir/f &&
  mv dir newdir`): the kernel emits a single R record for the directory
  rename. The CLI's tree builder handles the subtree move — all children's
  paths are updated automatically. No stale paths, no prefix rewriting,
  no kernel re-emission of child records.

### Checkpoint Segments

The CLI resolves per-checkpoint deltas by iterating over segments from the
`Journal` pipeline. Each segment is resolved independently by
building a dir tree from its records — records between
consecutive K/T markers form one segment. This is O(N) total.

K records a checkpoint. The CLI can slice segments with
`Journal::live_segments_slice(at, from, to)`, so that only the requested
range of segments is resolved and displayed.

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

1. Build a `Journal` from the journal records, then call `live_segments()` to get
   only reachable segments (filtering out dead branches from restores).
2. Replay live records in journal order directly on the base filesystem.
   Each record is applied one by one:
   - **A**: copy `inodes/<ino>` to `base/path` (create parents as needed;
     directories → `mkdir`).
   - **M**: copy `inodes/<ino>` to `base/path` (overwrite).
   - **D**: remove `base/path` (unlink for files/symlinks, rmdir for directories).
   - **R/P**: `rename(base/src, base/dst)`.
   No temp files, no sorting, no conflict detection — the temporal order
   from the journal is the correct replay order.
3. Clean up: remove all files under `.agfs/inodes/`, truncate `.agfs/journal`.
4. Signal kernel to reset staging state (`AGFS_IOC_RESTORE` with `target_gen=0`, `entry_count=0` — reset mode, no T record written).

**Abort** (`agfs abort`):

1. Count staged changes; if none, print "nothing to discard" and exit.
2. Prompt for confirmation: `Discard N staged changes? [y/N]`.
3. Remove all files under `.agfs/inodes/` and truncate `.agfs/journal`.
4. Signal kernel to reset staging state (`AGFS_IOC_RESTORE` with `target_gen=0`, `entry_count=0` — reset mode, no T record written).

**Status** (`agfs status`):

1. Build a `Journal` from the journal records and call `live_segments_slice()` to filter
   unreachable records and optionally narrow to a range (`--at`, `--from`, `--to`).
2. Build a dir tree from each segment's records independently.
3. Walk the tree for display: one-line summaries under checkpoint headers (and any trailing
   unsaved changes). Print total count.

**Diff** (`agfs diff`):

1. Build a `Journal` from the journal records and call `live_segments_slice()` to filter
   unreachable records and optionally narrow to a range (`--at`, `--from`, `--to`).
2. Build a dir tree from each segment's records independently.
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

`agfs checkpoint [name]` calls `ioctl(AGFS_IOC_CHECKPOINT)`. The kernel:

1. Returns `-ENOTSUP` if `staging` is disabled (checkpoints require staging).
2. Takes `staging_sem` write lock.
3. If `staging_fd_count > 0`, releases sem and returns `-EBUSY`.
4. Increments `sbi->gen` (atomic counter).
5. Appends `K\0<gen>\0<name>\n` to the journal.
6. Releases `staging_sem`.
7. Returns the gen to userspace.

No dirent tables change. No caches invalidated. The write lock on
`staging_sem` ensures no open-for-write is mid-flight when the generation
is bumped (open-for-write holds at least a read lock while incrementing
`staging_fd_count`).

The name defaults to `"after <cmd>"` when auto-checkpointing via `agfs exec`
(e.g., `"after make build"`), or a human-readable timestamp like
`chk-20260315-043807` when run via `agfs checkpoint` with no argument. Names need
not be unique; checkpoints can also be addressed by their numeric gen. When
looking up by name, `--at` and `--from` match the latest one.

Auto-checkpointing after `agfs exec` is skipped when the command produced no
staged changes. The kernel tracks a `dirty` flag on `agfs_sb_info` that is set
on every data journal write (A/M/D/R/P) and cleared on checkpoint or restore.
When the CLI passes the `AGFS_CHK_IF_CHANGED` flag in the checkpoint ioctl, the
kernel returns `gen = 0` (skipped) if the flag is clear, avoiding empty
checkpoints from read-only or no-op commands.

### Re-COW on First Open-for-Write After Checkpoint

The COW check is per-dirent: `agfs_pde_gen(de->packed)` records the
`sbi->gen` at which the current inode was created. `sbi->gen` starts
at 1. Newly created files set
`gen = sbi->gen` at creation time, so they are already
up-to-date and skip the COW check. Base files that have no dirent
(or are tombstoned) naturally trigger COW on the first
open-for-write.

At open time, the COW check in `agfs_open` (see [Open / Read / Write
Path](#open--read--write-path)) handles both base→staged COW and
staged→staged re-COW: if the dirent is missing, is a tombstone, or has a
stale `gen`, a fresh inode is created.

`agfs_do_cow` copies from the dentry's current `lower_path` — which is
the base file before any COW, or the current staged inode after one.
The same function handles both cases; no separate re-COW path.
`agfs_do_cow` also updates the gen field in the packed dirent after a successful COW.

Because no fd spans a checkpoint boundary (enforced by `staging_fd_count`),
the write and mmap paths need no COW checks — they are pure pass-throughs.

**Multiple checkpoints between opens** work naturally: if N checkpoints occur
without any open-for-write, the first open after them triggers one re-COW.
The generation counter collapses consecutive checkpoints.

### Example Journal with Checkpoints

```
M\0/src/main.rs\0f\01\n                          # COW: main.rs -> ino 1
A\0/src/lib.rs\0f\02\n                           # create lib.rs -> ino 2
K\01\0after make build\n                          # checkpoint 1
M\0/src/main.rs\0f\03\n                          # re-COW: main.rs -> ino 3
D\0/src/lib.rs\0f\n                               # delete lib.rs
A\0/src/new.rs\0f\04\n                           # create new.rs
K\02\0after make test\n                           # checkpoint 2
M\0/src/new.rs\0f\05\n                           # re-COW: new.rs -> ino 5
```

State at each point:

| Checkpoint         | main.rs | lib.rs    | new.rs |
|--------------------|---------|-----------|--------|
| "after make build" | ino 1   | ino 2     | --     |
| "after make test"  | ino 3   | (deleted) | ino 4  |
| current            | ino 3   | (deleted) | ino 5  |

### Example Journal with Restore

```
M\0/src/main.rs\0f\01\n                          # COW: main.rs -> ino 1
A\0/src/lib.rs\0f\02\n                           # create lib.rs -> ino 2
K\01\0after make build\n                          # checkpoint 1
M\0/src/main.rs\0f\03\n                          # re-COW: main.rs -> ino 3
D\0/src/lib.rs\0f\n                               # delete lib.rs
K\02\0after make test\n                           # checkpoint 2
T\03\01\n                                         # restore to checkpoint 1 (gen bumped to 3)
A\0/src/util.rs\0f\04\n                          # create util.rs -> ino 4
K\04\0after make fix\n                            # checkpoint 4
```

Reachable records (after `reachable`): M(main.rs→1), A(lib.rs→2), K1, A(util.rs→4), K4
Unreachable region: M(main.rs→3), D(lib.rs), K2, T3

State at current:

| Checkpoint         | main.rs | lib.rs | util.rs |
|--------------------|---------|--------|---------|
| "after make build" | ino 1   | ino 2  | --      |
| "after make fix"   | ino 1   | ino 2  | ino 4   |

### Checkpoint-Aware CLI Operations

All checkpoint query flags (`--at`, `--from`, `--to`) work by slicing
the journal records before resolving into segments. This means the
output always preserves checkpoint boundaries within the requested range.

**`agfs status`** / **`agfs diff`** support:
- `--at <name|gen>` — show the single segment at that checkpoint.
- `--from <name|gen>` — show changes from that checkpoint to end of journal.
- `--to <name|gen>` — show changes from start of journal to that checkpoint.
- `--from <A> --to <B>` — show changes between two checkpoints.
- `--at` conflicts with `--from`/`--to`.

**`agfs restore <name|gen>`**: Restore the mounted view to the state at the
named checkpoint. The journal is **append-only** — restore appends a T
record instead of truncating. T records create unreachable records — records
between the target checkpoint and the T record that no longer reflect
current state. All CLI consumers (commit, status, diff, restore) build a
`Journal` to filter unreachable records before resolving.

The reachability algorithm: O(N) single pass to collect T/K positions,
O(R) backward walk to build reachable ranges, skip unreachable T records.

1. CLI builds a `Journal` and finds the target checkpoint via
   `Markers::find_checkpoint()` (including unreachable regions, to support undo-restore).
2. CLI calls `live_segments_at(gen_id)` (or `live_segments_at_name(name)` which
   resolves the name internally) to get an iterator over live segments in the
   prefix up to the target checkpoint, handling any T records in that
   prefix.
3. CLI builds the dir tree from live records → dirents.
4. CLI converts the dir tree to dirent entries (path, ino, base, d_type).
   Entries are sorted by path — parents before children — so that
   `vfs_path_lookup` in the kernel can find staged parent directories
   when injecting child dirents.
5. `ioctl(AGFS_IOC_RESTORE, { target_gen=N, entries })`:
   kernel wipes all dirents, shrinks dcache, injects entries with
   new gen, appends `T\0<new_gen>\0<target_gen>\n`, returns new_gen.
   The `AGFS_IOC_RESTORE` ioctl **increments** gen (monotonically)
   instead of setting it to the target value — this avoids gen
   collisions. Injected dirents receive the new gen value.
6. No journal truncation.
7. Orphaned inodes (from post-checkpoint mutations) remain in the inode
   store — cleaned up on the next `commit` or `abort`.

After restore, future writes trigger re-COW only if a new checkpoint is
taken (since gen is bumped to a fresh value by the restore ioctl). Editing
files without a new checkpoint reuses the existing inodes in place.

**`agfs timeline`**: Show the checkpoint/restore DAG in chronological
order, with unreachable branches dimmed. **`agfs audit`**: Show every
raw journal record (unreachable dimmed), with an optional `--path` filter
to trace operations on a specific file.
