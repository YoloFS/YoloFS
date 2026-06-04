# Staging-Commit Layer

The staging layer intercepts all writes and redirects them to a sharded
inode store (`.yolofs/inodes/<shard>/<ino>`). Changes are invisible to the
lower filesystem until an explicit `commit`. An `abort` discards them
instantly.

## Design Invariants

Two invariants simplify the kernel-side design. Both are **enforced** in
the kernel, not merely assumed.

1. **No open file handles during snapshot.** The snapshot ioctl rejects
   with `-EBUSY` if any staging fds are open (`sbi->staging_fd_count > 0`).
   The CLI naturally satisfies this by taking snapshots between `yolo exec`
   invocations, never while agent processes are running. This means
   `sbi->gen` cannot change while any staging fd is open, so the
   COW decision can be made once at `open()` time — the write and mmap
   paths are pure pass-throughs with zero staging logic.

2. **Staged child dentries are pinned.** When the first
   staged entry is added to a directory, the child dentry is pinned with
   `dget()`, keeping it in the dcache. The overlay state
   (`target` + `pinned` in `yolo_dentry_info`) lives on the VFS dentry
   (via `d_fsdata`), so it survives dentry cache pressure naturally.
   Pinned child dentries hold a ref on `dentry->d_parent`, which
   transitively keeps the parent inode alive through
   VFS refcounting — no `igrab()` is needed. Pins are released in bulk by
   `YOLO_IOC_TRAVEL` (called by commit/abort/travel) and during
   `kill_sb` (unmount) via a recursive dentry tree walk from
   `sb->s_root`. Directories are never COW'd, so their inode
   identity (keyed by `lower_inode` in `iget5_locked`) is stable for
   the entire staging session.

## Concepts

| Term                  | Meaning |
| --------------------- | ------- |
| **base**              | Always `/` — the entire root filesystem, read-only from YoloFS's perspective until commit. |
| **inode store**       | `.yolofs/inodes/` — a sharded store of inodes. Each entry lives under a shard directory: `inodes/<ino/1000>/<ino>` (e.g., `inodes/0/1`, `inodes/0/2`, ..., `inodes/1/1000`). Shards keep each directory small (~1000 entries) for fast ext4 htree lookups. Regular files and symlinks are stored as inodes; directories created by `mkdir` are empty directory inodes (children live in their own entries). No mirrored directory tree. |
| **staged dentry list** | Per-directory set of pinned staged VFS dentries, identified by `pinned == true` in the VFS `d_children` list. Each pinned dentry carries overlay state in its `yolo_dentry_info`. Records which children are added, modified, deleted, or renamed. This is the kernel's source of truth. |
| **journal**           | `.yolofs/journal` — append-only log of all mutations. Written by the kernel, read by the CLI for commit/abort/review. The kernel never reads it back. |
| **mount point**       | `.yolofs/mnt/` — the agent's view of the filesystem. Shows the merged base + staged changes with permission gating applied. |
| **commit**            | CLI reads the journal and applies all operations to the base filesystem. |
| **abort**             | CLI deletes journal + inode store. O(1). |

## Inode Store Ownership

The inode store directory (`.yolofs/inodes/`) and journal file (`.yolofs/journal`)
are created by the CLI during `yolo mount` and are owned by the calling user.
This means all kernel-side VFS operations on the inode store (create, mkdir,
open, lookup) run under the user's own credentials — no credential override
is needed. The YoloFS permission gating layer controls access through the mount
point separately (see [permissions.md](permissions.md)).

## Storage Layout

```
yolofs.toml                       # config file in CWD (mount options + rules)
.yolofs/                          # created by `yolo` in CWD
├── journal                      # append-only journal (all ops)
├── inodes/                      # sharded inode store
│   ├── 0/                       # shard 0 (inodes 0–999)
│   │   ├── 1                    # inode: content of some file
│   │   ├── 2                    # inode: content of another file
│   │   └── ...
│   ├── 1/                       # shard 1 (inodes 1000–1999)
│   │   └── ...
│   └── ...
└── mnt/                         # mount point -- agent works here
    └── .ctl                     #   synthetic control file for ioctls
```

## In-Kernel State

All staging state lives in the structures below. Nothing is shared
across mounts. The two design invariants (no open fds during snapshot,
staged child dentries pinned) keep the state minimal: the file struct carries
zero staging-specific fields; all staging truth lives in the overlay
state on pinned VFS dentries, identified by `pinned == true` in the
VFS `d_children` list.

**Per-superblock** (`yolo_sb_info`) — one instance, lives for the mount:

| Field | Purpose |
|-------|---------|
| `inodes_dir` | Pinned path to `.yolofs/inodes/` |
| `journal_file` | Open file handle to `.yolofs/journal` (`O_APPEND`). The kernel only appends; it never reads the journal back. |
| `staging_sem` | rw\_semaphore serializing COW / re-COW, snapshot, and journal writes |
| `next_ino` | Atomic counter for inode names (`1`, `2`, …) |
| `gen` | Atomic counter, starts at 1, bumped by each snapshot and travel ioctl. Compared against `staging_gen` on the inode at open time to decide COW / re-COW. |
| `staging_fd_count` | Atomic counter of open staging fds (opened for write). Snapshot ioctl rejects with `-EBUSY` when > 0. |
| `dirty` | Boolean flag set on every data journal write (S/D/R), cleared on snapshot or travel. Used by `YOLO_SNAPSHOT_IF_CHANGED` to skip empty auto-snapshots. |

**Per-inode** (`yolo_inode_info`) — one per cached inode:

| Field | Purpose |
|-------|---------|
| `lower_inode` | Pointer to the lower-FS inode (base file at lookup time). Not updated after COW — stale but harmless. Used only for `evict_inode` cleanup and directory permission pass-through. |
| `staging_gen` | `u16` COW generation. Set to `sbi->gen` when the inode is staged or COW'd. Compared at open time to decide COW / re-COW. |

The staging state lives on the VFS dentries (via `d_fsdata`), not on the
directory inode. Directories are never COW'd, so their inode identity is
stable for the entire staging session. The COW generation for regular
files is tracked on the inode (`staging_gen` in `yolo_inode_info`), not
on the dentry.

**Per-dentry** (`yolo_dentry_info`) — one per cached dentry:

| Field | Purpose |
|-------|---------|
| `lower_path` | Resolved path to the backing file for positive dentries — either `inodes/<ino>` or the base file. Updated in-place by COW. Redirect entries point at the base source. Lookup-miss negatives and tombstones keep `lower_path` empty. |
| `target` | `enum yolo_target`: **INODE** (1 — staged in `inodes/<ino>`), **PATH** (2 — zero-copy rename redirect to base path), **NONE** (3 — no target). |
| `pinned` | `bool` — whether this dentry is pinned (staged). A dentry is staged iff `pinned == true`. Ground state: `target=PATH, pinned=false`. |

**Per-file** (`yolo_file_info`) — one per open file descriptor:

| Field | Purpose |
|-------|---------|
| `lower_file` | Open file handle to the lower file. Always points at the correct inode (COW is resolved at open time, not deferred to write). |

No staging-specific flags. Because no fd spans a snapshot boundary
(enforced by `staging_fd_count`), the file handle established at open
time is valid for the lifetime of the fd.

## Path Resolution

### Dentry State

Two fields define dentry state (`yolo_dentry_info`):

- `target` (`enum yolo_target`): **INODE** (1) — content staged in `inodes/<ino>`.
  **PATH** (2) — zero-copy rename redirect to a base path via `lower_path.dentry`.
  **NONE** (3) — no target (ground state or tombstone).
- `pinned` (`bool`): whether the dentry is staged.

A dentry is staged iff `pinned == true`. The ground state is
`target=PATH, pinned=false` — the dentry follows the base filesystem.
Set by `d_init` and `yolo_dentry_unpin`.

- **Staged inode**: `target=INODE, pinned=true`. Content staged in `inodes/<ino>`.
- **Redirect**: `target=PATH, pinned=true`. Zero-copy rename redirect
  to a base path via `lower_path.dentry`.
- **Tombstone**: `target=NONE, pinned=true`. Hides any base entry at this
  path. `NONE` exclusively means tombstone — ground state uses `PATH`.

Only positive staged dentries carry a populated `lower_path`. Ground-state
lookups that miss in base and pinned tombstones both remain negative dentries
with an empty `lower_path`; later VFS operations must not assume a negative
dentry can be reopened through `lower_path`.

The `d_type` is derived on-the-fly from `d_inode(dentry)->i_mode` via
`fs_umode_to_dtype()` for readdir only. It is not stored in the dentry
state or the journal.

Callers read `YOLO_D(d)->target` and `YOLO_D(d)->pinned` directly.

**Wire format**: The travel ioctl serializes dentry state as a `tag:u8`
followed by variant-specific payload. Tags match `yolo_target` values:
INODE → `ino:le32`; PATH → `path_len:le16 path:u8[path_len]`;
NONE → nothing. Tombstones are identified by `target == NONE`.
Passthrough dirs are encoded as PATH with `path_len=0`.
The full recursive travel tree format is documented in
`docs/plans/33-dentry-state-redesign.md`.

**Lookup** (`yolo_lookup`) — called by the VFS when no cached dentry
exists. All staged entries are pinned in the dcache, so `lookup_fast()`
finds them before `->lookup()` is ever called. When `->lookup()` runs,
the name is guaranteed to be unstaged — it falls through directly to
the base filesystem. A base hit stores the resolved backing path on the
positive dentry; a base miss adds a negative dentry and drops the temporary
lower lookup ref immediately:

```
yolo_lookup(dir, dentry):
    # d_init already set up d_fsdata — no manual allocation needed.
    lower = base_lookup(dir, dentry->d_name)   # returns a positive or negative dentry
    lower_path = { dentry: lower, mnt: lower_mnt }
    interpose(dentry, lower_path)
    if dentry is positive:
        cache_perm(dentry)
    return NULL
```

**Readdir** merges the staged dentries with the base directory:

```
yolo_readdir(dir):
    # Phase 1: walk d_children with d_lock pin-and-release pattern.
    for child in parent_dentry->d_children (via d_lock):
        if !YOLO_D(child)->pinned:
            continue
        if YOLO_D(child)->target == YOLO_TARGET_NONE:
            continue    # negative dentry
        dir_emit(child->d_name,
                 d_inode(child)->i_ino,
                 fs_umode_to_dtype(d_inode(child)->i_mode))
    # Phase 2: emit base entries not overridden.
    for entry in base_readdir(dir):
        result = d_lookup(dir_dentry, &entry.name)
        if result and YOLO_D(result)->pinned:
            dput(result)
            continue   # overridden by staged entry
        if result:
            dput(result)
        dir_emit(entry.name)
```

The staged dentry list is the kernel's in-memory source of truth. The
journal persists it on disk for the CLI. The kernel never reads the
journal back.

## Open / Read / Write Path

The backing file (staged inode or base file) is determined at **lookup**
time via the dcache. `open()` receives a dentry already pointing at
the right lower inode.

COW and re-COW are resolved at **open time**, not deferred to the first
write. Because no fd spans a snapshot boundary (enforced by `staging_fd_count`),
`sbi->gen` is stable for the lifetime of the fd, so the decision
made at open time is final. This makes `write_iter` and `mmap` pure
pass-throughs with zero staging logic.

Staging publications that involve COW or re-COW (staging a dentry,
updating the dentry's lower path, and appending the journal
record) are serialized under `staging_sem` and must succeed as a unit.
If any step fails (e.g., journal append), the operation fails and the
previous mapping remains authoritative.
Create/mkdir/symlink/unlink/rmdir/rename are already serialized by the VFS
`inode_lock(dir)` and do not need `staging_sem`.

```
yolo_open(inode, file):
    if file->f_flags & (O_WRONLY | O_RDWR):
        if YOLO_D(dentry)->target == YOLO_TARGET_INODE && YOLO_I(d_inode(dentry))->staging_gen >= sbi->gen:
            // Inode is current — open it directly (O_TRUNC truncates in place).
            down_read(staging_sem)
            atomic_inc(staging_fd_count)
            up_read(staging_sem)
            file_info->lower_file = yolo_open_staged_lower(dentry, file->f_flags)

        else:
            // Needs COW (base file, redirect, or stale inode).
            // yolo_do_cow copies from dentry's current lower_path
            // to a fresh inode. With O_TRUNC, creates an empty inode.
            down_write(staging_sem)
            // Re-check kind under sem — a concurrent open may have
            // already COW'd this file since our check above.
            atomic_inc(staging_fd_count)
            new_file = yolo_do_cow(sbi, dentry, flags, O_TRUNC in flags)
            // yolo_do_cow updates: dentry target,
            //   YOLO_I(inode)->staging_gen, dentry lower_path, journal
            up_write(staging_sem)
            file_info->lower_file = new_file

    else:
        // Read-only: open the lower file the dentry points to.
        file_info->lower_file = open(lower_file, O_RDONLY)

yolo_release(inode, file):
    if file was opened for write:
        atomic_dec(staging_fd_count)

yolo_write_iter(kiocb, iov_iter):
    // Pure pass-through — COW already resolved at open time.
    lower_file->f_op->write_iter(...)

yolo_fallocate(file, mode, offset, len):
    // Pass-through to lower file if supported by the lower fs.
    lower_file->f_op->fallocate(lower_file, mode, offset, len)

yolo_mmap(file, vma):
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

All creation operations allocate a new inode and stage the dentry with
`target = YOLO_TARGET_INODE` and `staging_gen = sbi->gen` so the
file is recognised as already staged (preventing a spurious re-COW on
next open-for-write):

```
yolo_create(dir, dentry, mode):
    ino = next_ino++
    create file inodes/<ino>
    yolo_dentry_pin(dentry, YOLO_TARGET_INODE)
    YOLO_I(d_inode(dentry))->staging_gen = sbi->gen
    journal(S, path, ino)

yolo_mkdir(dir, dentry, mode):
    ino = next_ino++
    create dir inodes/<ino>/
    yolo_dentry_pin(dentry, YOLO_TARGET_INODE)
    YOLO_I(d_inode(dentry))->staging_gen = sbi->gen
    journal(S, path, ino)

yolo_symlink(dir, dentry, target):
    ino = next_ino++
    create symlink inodes/<ino> -> target
    yolo_dentry_pin(dentry, YOLO_TARGET_INODE)
    YOLO_I(d_inode(dentry))->staging_gen = sbi->gen
    journal(S, path, ino)
```

`touch` (create + close, no write) produces an empty inode in the store —
visible in `yolo review` (`--diff` for the body) and cleanly discarded by abort.

## Delete / Rmdir Path

Delete creates a tombstone (negative dentry) and appends to the journal.
The kernel always tombstones regardless of whether the path had base
content — a spurious tombstone is harmless and cleaned up on commit/reset.

```
yolo_unlink(dir, dentry):
    staged = YOLO_D(dentry)->pinned

    # Pre-allocate negative dentry before journal so we can fail cleanly.
    tomb = yolo_dentry_create(parent, name, namelen, YOLO_TARGET_NONE, NULL)
    if IS_ERR(tomb): return PTR_ERR(tomb)

    journal(D, path)  # must be before d_drop (uses dentry path)

    if staged:
        yolo_dentry_unpin(dentry)   # reset to ground state + dput
    d_drop(dentry)

yolo_rmdir(dir, dentry):
    # Same logic as yolo_unlink.
    journal(D, path)
```

Subsequent lookup of the name finds the negative dentry in the dcache
and returns it as a negative dentry. The base file is untouched until commit.

## Rename Handling

Rename is decomposed into a delete of the old name + creation at the new
name. No file content is copied — only dentry metadata changes.

All renames — staged or redirect, file or directory — emit a single R
record.

```
yolo_rename(old_parent, old_dentry, new_parent, new_dentry):
    dst = join(new_parent, new_name)
    src = join(old_parent, old_name)

    # Always tombstone at the old name, pre-allocate before
    # any irreversible changes.
    tomb = yolo_dentry_create(old_parent, old_name, old_namelen, YOLO_TARGET_NONE, NULL)
    if IS_ERR(tomb): return PTR_ERR(tomb)

    # Build destination state.
    if YOLO_D(old_dentry)->target == YOLO_TARGET_INODE:
        # Carry forward the staged inode.
        yolo_dentry_pin(old_dentry, YOLO_TARGET_INODE)
    elif YOLO_D(old_dentry)->target == YOLO_TARGET_PATH:
        # Carry forward the redirect — base source is already in lower_path.
        yolo_dentry_pin(old_dentry, YOLO_TARGET_PATH)
    else:
        # Base file being renamed — becomes a redirect via lower_path.
        yolo_dentry_pin(old_dentry, YOLO_TARGET_PATH)

    journal(R, dst, src)

    # Clean up new_dentry if it was staged.
    new_staged = YOLO_D(new_dentry)->pinned
    if new_staged:
        yolo_dentry_unpin(new_dentry)

    # Handle roundtrip detection.
    if is_roundtrip:
        yolo_dentry_unpin(old_dentry)     # cancel — back to ground state

    d_drop(old_dentry)
    d_drop(new_dentry)
```

**Rename chains** (`mv a->b`, then `mv b->c`) work naturally: the second
rename finds the redirect state on `b`'s dentry, uses its dentry path as the
old path, and sets a new redirect on `c`'s dentry. The base source is
derived from `lower_path.dentry` — each rename is
recorded as a separate journal entry (the DirTree collapses chains into
minimal redirects at commit time).
**Roundtrip renames** (`mv a->tmp`, then `mv tmp->a`) are detected at
rename time: when the effective base source equals the destination
relpath, the rename chain is a no-op. The kernel
leaves the destination dentry as unpinned. The journal still records
the R entries (the CLI has its own roundtrip detection). Negative dentries at
intermediate positions (e.g. `/tmp` in a swap via third path) are
independently correct and unaffected.

**Rename + recreate** (`mv a->b`, then `touch a`) works because the new
`touch a` sees the tombstone pinned in the dcache and creates a new
staged inode that supersedes it. The rename emits `R(b, a)`, so the
journal sees `R(b, a)` for the rename, then `S(a)` for the recreate.

**Read after rename**: lookup of the new name finds the pinned dentry
in the dcache -> opens the base file via `lower_path` (or the
staged inode). Lookup of the old name finds the negative dentry ->
returns `-ENOENT` (or falls through to base if cancelled).

**Write after rename**: opening for write triggers COW at open time. The
base file is copied into a new inode; the dentry's target changes
from `YOLO_TARGET_PATH` to `YOLO_TARGET_INODE`.

Commit and abort handling for renames is covered in
[Staging Operations](#staging-operations-userspace).

## Readdir (Merged Directory Listing)

`readdir` (`iterate_shared`) presents a merged view: staged entries
first, then base entries that aren't overridden.

**Fast path — staging disabled**: when staging is disabled or the
inodes directory is not set up (`!sbi->staging || !sbi->inodes_dir.dentry`),
the merge is unnecessary — every base entry passes through unchanged. In
this case `yolo_readdir` delegates directly to
`iterate_dir(lower_file, ctx)`, identical to the non-staging path. The
lower filesystem manages `f_pos` itself, so the cost matches native.

**Merge path — staging enabled**:

```
yolo_readdir(dir, ctx):
    if !sbi->staging || !sbi->inodes_dir.dentry:
        return iterate_dir(lower_file, ctx)   # fast path

    # Phase 1 (yolo_emit_dirents): keep a per-open cursor dentry in
    # d_children and resume from cursor successor on repeated calls.
    # This is robust against sibling-list mutations.
    for child in staged_children_from(cursor_or_head):
        if !YOLO_D(child)->pinned or YOLO_D(child)->target == YOLO_TARGET_NONE:
            continue
        dir_emit(ctx, child->d_name,
                 d_inode(child)->i_ino,
                 fs_umode_to_dtype(d_inode(child)->i_mode))

    # Phase 2 (yolo_fill_base): emit base entries not overridden by
    # staged entries. Uses d_lookup() (O(1) dcache hash) per base
    # entry to check if overridden.
    lower_file.f_pos = file_info.base_pos   # resume, not restart
    for entry in base_readdir(dir):
        result = d_lookup(dir_dentry, &entry.name)
        if result and YOLO_D(result)->pinned:
            dput(result)
            continue   # overridden by staged entry
        if result:
            dput(result)
        dir_emit(ctx, entry.name)
    file_info.base_pos = lower_file.f_pos   # save for next call
```

Staged entries are emitted with the correct `d_type` (DT_REG, DT_DIR,
DT_LNK) derived from `d_inode->i_mode`. The `ino` passed to `dir_emit`
is `d_inode(child)->i_ino` for all non-negative staged entries.

The merged list is built fresh on every `readdir` call — no caching.
This ensures creates, deletes, and renames between `getdents64` calls
are always visible. The staged dentry list is small, so the cost is
negligible.

**Position tracking**: the merge path must not reset the lower file's
`f_pos` to 0 on each `getdents64` call. Doing so forces the lower
filesystem (e.g. ext4 htree) to re-read and re-sort the entire
directory from scratch on every call, turning readdir into an O(n²)
operation. Instead, `yolo_file_info` stores `base_pos` (the lower
`f_pos` at the end of the previous phase-2 pass) and `dirent_off` (the
number of virtual entries counted in phase 1). On re-entry,
phase 1 is skipped once all staged entries have been emitted (`off`
starts at the saved `dirent_off`), and phase 2 resumes from `base_pos`.

## setattr / getattr / fsync

**`setattr`** (e.g., `chmod`, `chown`, `truncate`): `ATTR_MODE` is always
stripped (chmod is a no-op through YoloFS). When staging is active,
`ATTR_SIZE` is stripped — truncation is handled by the COW mechanism at
open time (O_TRUNC creates an empty inode), not by setattr. Remaining
attribute changes (e.g., `chown`, timestamps) propagate directly to the
lower file (staged inode if COW'd, base file otherwise). No COW is
triggered by setattr itself.

**`getattr`** (e.g., `stat`): Stats from the resolved path — the staged
inode if the file has been modified, otherwise the base file.

**`fsync`**: If the file is staged (`YOLO_D(dentry)->target == YOLO_TARGET_INODE`),
returns 0 immediately — staged inodes are ephemeral and will be committed or
discarded as a batch. For base files opened read-only, fsync is delegated to
the lower filesystem as usual.

## Journal Format

The journal is an append-only file at `.yolofs/journal`. Each record is a
sequence of NUL-separated fields terminated by a newline.

```
S\0<path>\0<ino>\n                — Stage (staged content at path)
D\0<path>\n                      — Delete
R\0<dst>\0<src>\n                 — Rename
P\0<gen>\0<name>\n                — Snapshot
T\0<gen>\0<target_gen>\n          — Travel
A\0<path>\0<op>\0<decision>\n    — Ask resolved (records the decision)
B\0<path>\0<op>\n                — Blocked by a rule (permission denied)
```

Each mutation type has its own record tag and carries exactly the fields
it needs. The kernel always uses `S` for creates/COW and `R` for renames:

| Tag | Fields | Meaning |
|-----|--------|---------|
| `S` | `<path>`, `<ino>` | Staged content at path (create or COW) |
| `D` | `<path>` | Entry deleted |
| `R` | `<dst>`, `<src>` | Rename |
| `P` | `<gen>`, `<name>` | Snapshot marker |
| `T` | `<gen>`, `<target_gen>` | Travel marker |
| `A` | `<path>`, `<op>`, `<decision>` | An `ask` was resolved to `<decision>` — observational |
| `B` | `<path>`, `<op>` | Access blocked by a rule (`-EACCES`) — observational |

`op` is a single letter (`r`/`w`); `decision` is a single letter
(`a`/`y`/`r`/`d`/`h` — ask/allow/read/deny/hide).
S/D/R are state mutations. P/T are control markers. **A and B are
observational notes**: they record that a rule blocked an access (`B`) or
that an `ask` was resolved (`A`, by the daemon or the timeout default) but
do not affect any state. The CLI's dir-tree builder, commit, abort, and review
ignore them; only `yolo journal` surfaces them. A/B writes do not set
`sbi->dirty`, so a command that only triggers blocks/asks does not cause an
auto-snapshot under `YOLO_SNAPSHOT_IF_CHANGED`. They ride within segments
alongside S/D/R, so reachability and `-- <path>` filtering
apply identically (a B in an unreachable segment is dimmed in journal
output). Current scope is `-EACCES` only; `HIDE`/`-ENOENT` paths are
not logged.

Userspace derives the stage/modify distinction by checking the base
filesystem — it does not need the kernel to encode it in the tag.

**Gen_id invariant.** The kernel increments `sbi->gen` via
`atomic_inc_return()` on every P and T record. Gen_id values are
strictly sequential: marker\[i\] has gen_id = i (marker\[0\] is a phantom
`Snapshot { gen_id: 0, name: "(initial)" }` inserted by the CLI). The
`MarkerIndex` type relies on this for O(1) snapshot lookup by gen_id.

`<path>` is the full overlay path (e.g. `/dir/file`).
`<src>` is the overlay path before the rename (R only).
`<ino>` is the staged inode ID (decimal).

All renames — staged or redirect, file or directory — emit a single R
record. The tree builder always tombstones at the source path.

### Snapshot Segments

The CLI resolves per-snapshot deltas by iterating over segments from the
`Journal` pipeline. Each segment is resolved independently by
building a dir tree from its records. Marker\[i\] opens segment\[i\]:
segment\[i\] contains the records from marker\[i\] up to (but not
including) marker\[i+1\]. The phantom marker at index 0 opens segment 0
(pre-first-snapshot records). This is O(N) total.

A P record names a snapshot. `review` and `journal` take a positional
id/range spec (`[<id>|a..b|all]`); `parse_range` resolves it to a half-open
segment range `[start, end)` via `MarkerIndex::segment_range`, and
`Journal::live_segments_range(start, end)` yields just those (live) segments
to resolve and display.

Range semantics (the spec → `[start, end)`):
- `<id>` — that snapshot's own change: the single segment it sealed
  (`prev(id)..id`).
- `<a>..<b>` — the span between two snapshots; an empty end means base (`0`)
  or the tip.
- `all` (== `..` == `0..`) — every segment, base to tip.
- (omitted) — the latest segment (vs prev); the whole session under `--each`.

Written by the kernel (`kernel_write()` per mutation). Read by the CLI
for commit/abort/review. Never read by the kernel.

## Staging Operations (Userspace)

Commit and abort are **userspace operations** — the kernel module only handles
I/O redirection. The `yolo` CLI reads the journal and applies or discards.

**Commit** (`yolo commit`):

1. Build a `Journal` from the journal records, then build a `DirTree` from all
   live segments. The tree collapses redundant operations (e.g. create-then-delete,
   overwrite chains, rename chains) into a minimal set of final-state entries.
2. Walk the `DirTree` and collect commit ops into two groups:
   - **Renames**: `BasePath(src)` redirects &rarr; two-phase save/place
     through temp paths. All sources are saved deepest-first, then placed
     at destinations in DFS order. Handles swaps and rotation cycles
     automatically.
   - **Ops**: `Tombstone` entries &rarr; `remove(base/path)`;
     `StagedFile(ino)` entries &rarr; copy `inodes/<ino>` to `base/path`.
     Interleaved in DFS order.
3. Apply in order: saves &rarr; places &rarr; ops (deletes+stages).
   Principle: readers before writers — renames read from base paths, so
   saves must complete before any destination is written.
4. Clean up: remove all files under `.yolofs/inodes/`, truncate `.yolofs/journal`.
5. Signal kernel to reset staging state (`YOLO_IOC_TRAVEL` with `target_gen=0`,
   `tree_len=0` &mdash; reset mode, no T record written).

**Abort** (`yolo abort`):

1. Count staged changes; if none, print "nothing to discard" and exit.
2. Prompt for confirmation: `Discard N staged changes? [y/N]`.
3. Remove all files under `.yolofs/inodes/` and truncate `.yolofs/journal`.
4. Signal kernel to reset staging state (`YOLO_IOC_TRAVEL` with `target_gen=0`, `tree_len=0` — reset mode, no T record written).

**Review** (`yolo review`, `yolo review --diff`):

1. Build a `Journal` from the journal records; `parse_range` resolves the
   id/range spec to a half-open segment range `[start, end)`, skipping
   unreachable segments.
2. `Changeset::collect` makes one O(segment) pass over the range, gathering the
   observational notes and, per path, the pre-image from its *first* touch (the
   range-start version), then replays the range into one dir tree for the net
   per-path target.
3. Summary view: classify each net change from its pre-image alone — added (no
   pre-image), modified (pre-image differs), deleted (tombstone over a
   pre-image), or renamed — one line each. No previous-tree rebuild, no base
   stat: O(segment), not O(journal).
4. `--diff` view: for each change, diff its pre-image (old content) against the
   staged `inodes/<ino>` (new content) as a git-style unified hunk; renames show
   metadata, deletes show the removed content.
5. `--each` expands the range into one stanza per consecutive snapshot (the tip
   — work past the last snapshot — is headed `working`).

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

`yolo snapshot [name]` calls `ioctl(YOLO_IOC_SNAPSHOT)`. The kernel:

1. Returns `-ENOTSUP` if `staging` is disabled (snapshots require staging).
2. Takes `staging_sem` write lock.
3. If `staging_fd_count > 0`, releases sem and returns `-EBUSY`.
4. Increments `sbi->gen` (atomic counter).
5. Appends `P\0<gen>\0<name>\n` to the journal.
6. Releases `staging_sem`.
7. Returns the gen to userspace.

No staged dentries change. No caches invalidated. The write lock on
`staging_sem` ensures no open-for-write is mid-flight when the generation
is bumped (open-for-write holds at least a read lock while incrementing
`staging_fd_count`).

The name defaults to `"after <cmd>"` when auto-snapshotting via `yolo exec`
(e.g., `"after make build"`), or a human-readable timestamp like
`chk-20260315-043807` when run via `yolo snapshot` with no argument. Names need
not be unique; `review` and `journal` address snapshots by their numeric gen id
only (`0` is the base), so a duplicate name never makes a range ambiguous.

Auto-snapshotting after `yolo exec` is skipped when the command produced no
staged changes. The kernel tracks a `dirty` flag on `yolo_sb_info` that is set
on every data journal write (S/D/R) and cleared on snapshot or travel.
When the CLI passes the `YOLO_SNAPSHOT_IF_CHANGED` flag in the snapshot ioctl, the
kernel returns `gen = 0` (skipped) if the flag is clear, avoiding empty
snapshots from read-only or no-op commands.

### Re-COW on First Open-for-Write After Snapshot

The COW check is per-inode: `YOLO_I(d_inode(dentry))->staging_gen`
records the `sbi->gen` at which the current inode was staged or COW'd.
`sbi->gen` starts at 1. Newly created files set
`staging_gen = sbi->gen` at creation time, so they are already
up-to-date and skip the COW check. Base files that have no staged
dentry (or have negative dentries) naturally trigger COW on the first
open-for-write.

At open time, the COW check in `yolo_open` (see [Open / Read / Write
Path](#open--read--write-path)) handles both base→staged COW and
staged→staged re-COW: if the target is not `INODE`, or has a
stale `staging_gen`, a fresh inode is created.

`yolo_do_cow` copies from the dentry's current `lower_path` — which is
the base file before any COW, or the current staged inode after one.
The same function handles both cases; no separate re-COW path.
`yolo_do_cow` also sets target on the dentry and `staging_gen`
on the inode after a successful COW, and pins the dentry with `dget()`
if not already staged.

Because no fd spans a snapshot boundary (enforced by `staging_fd_count`),
the write and mmap paths need no COW checks — they are pure pass-throughs.

**Multiple snapshots between opens** work naturally: if N snapshots occur
without any open-for-write, the first open after them triggers one re-COW.
The generation counter collapses consecutive snapshots.

### Example Journal with Snapshots

```
S\0/src/main.rs\0f\01\n                          # COW: main.rs -> ino 1
S\0/src/lib.rs\0f\02\n                           # create lib.rs -> ino 2
P\01\0after make build\n                          # snapshot 1
S\0/src/main.rs\0f\03\n                          # re-COW: main.rs -> ino 3
D\0/src/lib.rs\0f\n                               # delete lib.rs
S\0/src/new.rs\0f\04\n                           # create new.rs
P\02\0after make test\n                           # snapshot 2
S\0/src/new.rs\0f\05\n                           # re-COW: new.rs -> ino 5
```

State at each point:

| Snapshot         | main.rs | lib.rs    | new.rs |
|--------------------|---------|-----------|--------|
| "after make build" | ino 1   | ino 2     | --     |
| "after make test"  | ino 3   | (deleted) | ino 4  |
| current            | ino 3   | (deleted) | ino 5  |

### Example Journal with Travel

```
S\0/src/main.rs\0f\01\n                          # COW: main.rs -> ino 1
S\0/src/lib.rs\0f\02\n                           # create lib.rs -> ino 2
P\01\0after make build\n                          # snapshot 1
S\0/src/main.rs\0f\03\n                          # re-COW: main.rs -> ino 3
D\0/src/lib.rs\0f\n                               # delete lib.rs
P\02\0after make test\n                           # snapshot 2
T\03\01\n                                         # travel to snapshot 1 (gen bumped to 3)
S\0/src/util.rs\0f\04\n                          # create util.rs -> ino 4
P\04\0after make fix\n                            # snapshot 4
```

Reachable records (after `reachable`): S(main.rs→1), S(lib.rs→2), P1, S(util.rs→4), P4
Unreachable region: S(main.rs→3), D(lib.rs), P2, T3

State at current:

| Snapshot         | main.rs | lib.rs | util.rs |
|--------------------|---------|--------|---------|
| "after make build" | ino 1   | ino 2  | --      |
| "after make fix"   | ino 1   | ino 2  | ino 4   |

### Snapshot-Aware CLI Operations

The positional id/range spec works by slicing the journal records before
resolving into segments, so the output always preserves snapshot boundaries
within the requested range.

**`yolo review`** and **`yolo journal`** share the spec `[<id>|a..b|all]`:
- `<id>` — the single segment that snapshot sealed (`prev(id)..id`).
- `<a>..<b>` — changes between two snapshots; an empty end is base (`0`) or tip.
- `all` (== `..` == `0..`) — everything from base to tip.
- omitted — the latest segment (vs prev), or the whole session under `--each`.

**`yolo travel <name|gen>`**: Move the mounted view to the state at the
named marker (snapshot or travel). The journal is **append-only** —
travel appends a T record instead of truncating. T records create
unreachable records — records between the target marker and the T record that no longer reflect
current state. All CLI consumers (commit, review, journal, travel) build a
`Journal` to filter unreachable records before resolving.

The reachability algorithm: O(N) single pass to collect P/T positions,
O(R) backward walk to build reachable ranges, skip unreachable T records.

1. CLI builds a `Journal` and finds the target marker via
   `MarkerIndex::find_marker()` (including unreachable regions, to support undo-travel).
2. CLI calls `live_segments_at(gen_id)` (or `live_segments_at_name(name)` which
   resolves the name internally) to get an iterator over live segments in the
   prefix up to the target marker, handling any T records in that
   prefix.
3. CLI builds the dir tree from live records.
4. CLI serializes the dir tree into a contiguous byte buffer (depth-first,
   children sorted by name).
5. `ioctl(YOLO_IOC_TRAVEL, { target_gen=N, tree_buf })`:
   kernel releases all pinned staged dentries (recursive `d_children`
   tree walk from `sb->s_root`, `dput()` each), shrinks dcache,
   `vmalloc`s + `copy_from_user`s the tree buffer, walks it iteratively
   with a directory stack to inject VFS dentries with new gen (via
   `d_alloc()`, set target/pinned, `d_add()`, `dget()` to pin), appends
   `T\0<new_gen>\0<target_gen>\n`, returns new_gen.  The
   `YOLO_IOC_TRAVEL` ioctl **increments** gen (monotonically) instead
   of setting it to the target value — this avoids gen collisions.
   Injected inodes receive the new gen value in `staging_gen`.
6. No journal truncation.
7. Orphaned inodes (from post-snapshot mutations) remain in the inode
   store — cleaned up on the next `commit` or `abort`.

After travel, future writes trigger re-COW only if a new snapshot is
taken (since gen is bumped to a fresh value by the travel ioctl). Editing
files without a new snapshot reuses the existing inodes in place.

**`yolo timeline`**: Show the snapshot/travel DAG in chronological
order, with unreachable branches dimmed. **`yolo journal`**: Show every
raw journal record (unreachable dimmed), with an optional `-- <path>` filter
to trace operations on a specific file.
