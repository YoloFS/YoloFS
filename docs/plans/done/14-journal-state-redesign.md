# Plan 14 — Journal Redesign

## Problem

The current journal is an **event log**: the kernel appends a record for every
mutation (create, modify, delete, rename), and userspace replays events through a
multi-stage compaction pipeline (decompose → cancel → merge → collapse) to derive
the final state. This is complex:

- 7 record types with overlapping semantics (A/M split, R/P split)
- Rename chains require multi-pass resolution in userspace
- Compaction pipeline is ~700 lines of non-trivial logic
- Directory renames produce stale child paths (existing bug)

## Requirements

1. **Fast kernel writes.** Each VFS operation emits one record. No child
   traversal on directory rename — the kernel writes a single R record
   regardless of how many descendants exist.

2. **Reconstruct dirent state.** Replaying the journal through a tree builder
   must produce the full in-kernel dirent table (the dir tree relative to
   base). This is used by restore (inject dirents into kernel) and
   diff/status (display overlay state). Diff/status can also work from
   per-segment dir trees without building the full tree.

3. **Ordered commit replay.** Commit needs a sequence of operations to apply
   to base, not a dir tree. The journal's chronological order IS the correct
   replay order — no reordering needed. This is distinct from the dir tree
   because the dir tree is unordered state, while commit requires operations
   applied in the right sequence (e.g. stage a child before renaming its
   parent directory).

4. **Per-checkpoint diff.** Given two checkpoints, produce a diff showing what
   changed between them. This is purely journal-derived — no base filesystem
   access needed. A/M tags distinguish added from modified; R carries `src`
   for rename display.

## Proposed Approach

Simplify the journal to 5 data record types (A/M/D/R/P) with full paths, and replace
the compaction pipeline with a **dir tree builder**. The tree handles
directory renames correctly by construction — R/P records trigger subtree moves, so
children's paths are always up to date.

### Terminology

| Term | Definition |
|------|-----------|
| **Record** | A single line in the journal recording one operation |
| **Segment** | Records between two consecutive markers (K/T); one generation's writes |
| **Dir tree** | A tree representing overlay state, with dirents as leaves. Built by applying records sequentially: `dir_tree + record = dir_tree`. Two dir trees cannot be merged directly — records must be replayed onto a dir tree to extend it. Scoped by range: `dir_tree(segment N)` = one generation's changes; `dir_tree(A..B)` = changes between checkpoints; `dir_tree(all)` = full overlay state relative to base |
| **live / dead** | Segment liveness after restore marker filtering |

### Design Principles

1. **Minimal journal records.** Each record logs one operation with the minimum
   fields needed. A/M distinction is determined by the kernel at write time
   (zero cost — the kernel already checks dirent + base). A/M is relative to
   base existence: A means the path has no base content, M means it does.
   This is invariant across segments — an Added path remains A in all
   subsequent segments until committed. A→M for the same path is impossible.
2. **Tree is the dir tree (for diff/status/restore).** The tree is the data
   structure for resolving journal records into overlay state. Insert nodes,
   mark deletions, move subtrees. Diff/status/restore walk the tree directly.
   Commit does not use the tree — it replays records sequentially on base.
3. **R is a move operation.** R moves a node (file) or subtree (directory) from
   `src` to `dst`. Children of renamed directories follow automatically.
4. **No crash consistency.** The journal is not designed for crash recovery.

## Journal Format

Append-only file at `.agfs/journal`. NUL-separated fields, newline-terminated
records. Each operation record corresponds to one `agfs_dirent` mutation —
replaying the journal reconstructs the full dirent table.

Records are either operations (A/M/D/R/P) or markers (K/T):

```
A\0<path>\0<dtype>\0<ino>\n       — Add (new path)
M\0<path>\0<dtype>\0<ino>\n       — Modify (existing path)
D\0<path>\0<dtype>\n              — Delete
R\0<dst>\0<src>\0<dtype>\n        — Rename (destination is new)
P\0<dst>\0<src>\0<dtype>\n        — Replace (destination existed in base)
K\0<gen>\0<name>\n                — Checkpoint
T\0<gen>\0<target_gen>\n          — Restore
```

**Field order:** In all data records, the primary subject is the first
field. For R/P, `dst` is the destination and `src` is the source — this is the
reverse of the current `(old_dir, old_name, new_dir, new_name)` convention.

**R vs P:** R means the destination path did not exist in base. P means it did —
the tree builder tags the destination as having base content. While the moved
node occupies the position, it hides the base file. If the node is later moved
away, a Tombstone is placed to keep the base file hidden.

**S → T rename:** The restore marker is renamed from S to T to avoid ambiguity
with "Staging" / "Snapshot" terminology. T stands for "Time-travel."

### Field Definitions

| Field    | Type        | Description                                        |
|----------|-------------|----------------------------------------------------|
| `dst`    | UTF-8 str   | Destination overlay path (e.g. `/dir/file`)        |
| `src`    | UTF-8 str   | Source overlay path before the rename (R/P only)   |
| `dtype`  | char        | `f` (file), `d` (dir), `l` (symlink)               |
| `ino`    | ASCII u64   | Inode store ID in `.agfs/inodes/` (A/M only)       |
| `gen`    | ASCII u64   | Generation / checkpoint ID                         |
| `name`   | UTF-8 str   | Checkpoint name (K only)                           |
| `target_gen` | ASCII u64 | Target checkpoint generation (T only)            |

## Dir Tree Builder

The dir tree is a directory tree. Internal nodes are directories; leaves are
**dirents** — a 1:1 representation of the kernel's `agfs_dirent` table.

Journal record tags are **verbs** (Add, Modify, Delete, Rename) — they describe
operations. Dirent variants are **nouns** (Inode, Link, Tombstone) — they
describe state. The tree builder applies operations to produce state.

```rust
struct DirTree {
    nodes: HashMap<String, DirNode>,
}

enum DirNode {
    File(Dirent),
    Dir(Option<Dirent>, DirTree),
}

enum Dirent {
    Inode { ino: u64, dtype: DType, in_base: bool },
    Link { base_path: String, dtype: DType, in_base: bool },
    Tombstone { dtype: DType },  // implicitly in_base=true
}
```

`File(dirent)` = leaf. `Dir(Some(dirent), subtree)` = directory with its own
state. `Dir(None, subtree)` = intermediate directory created during path
walking, not part of the output.

This maps directly to kernel dirent states:

| Dirent variant | Journal tag | Kernel `agfs_dirent` state |
|---|---|---|
| Inode, in_base=false | A | `ino > 0`, `in_base=false` |
| Inode, in_base=true | M | `ino > 0`, `in_base=true` |
| Link, in_base=* | R/P | `ino = AGFS_INO_REDIRECT`, `base` set |
| Tombstone | D | `ino = AGFS_INO_DELETED` |

Nodes with a `Dirent` are part of the dir tree. `Dir(None, ..)` nodes are
intermediate directories created during path walking.

### Tree Operations

Processing records in journal order. Three rules govern `in_base` and Tombstones:

1. **`in_base` is set by the record tag:** A/R → false, M/P → true.
2. **Tombstone on vacate:** when R/P moves a node away from a base path,
   place a Tombstone at the vacated position.
3. **Cancellation:** D on a node with `in_base=false` → remove node
   (cancel). D on a node with `in_base=true` → Tombstone.

| Record | Tree operation |
|--------|---------------|
| A(path, dtype, ino) | Walk/create path. Set dirent to `Inode { ino, dtype, in_base: false }`. |
| M(path, dtype, ino) | Walk/create path. Set dirent to `Inode { ino, dtype, in_base: true }`. |
| D(path, dtype) | Find node. If `in_base=false`, remove node (cancel). Otherwise, set dirent to `Tombstone { dtype }`. |
| R(dst, src, dtype) | If node exists at `src`: detach, reattach at `dst`. If source `in_base=true`, Tombstone at `src`. Set `in_base=false`. For directories, entire subtree moves. Inode stays Inode. If no node at `src` (base-only): create `Link { base_path: src, dtype, in_base: false }` at `dst`, Tombstone at `src`. |
| P(dst, src, dtype) | Same as R, but set `in_base=true`. |

### Cancellation

Create-then-delete cancels naturally in the tree:

```
A(/a, f, ino=1)  →  insert node with Inode dirent
D(/a)            →  dirent is Inode { in_base: false } → remove (cancel)
```

Result: no node in tree. Net effect: nothing.

If the record is M (existing path), D does not cancel — M means the path
existed before, so D is a real delete:

```
M(/a, f, ino=1)  →  insert node with Inode { in_base: true } dirent
D(/a)            →  dirent is Inode { in_base: true } → set to Tombstone
```

Rename-then-delete of a base file:

```
R(/b, /a, f)     →  base-only: create Link { base_path: /a, in_base: false } at /b;
                     Tombstone at /a (source had base content)
D(/b)            →  dirent has in_base=false → remove node (cancel)
```

Result: Tombstone at /a, nothing at /b. Base /a is hidden; /b has no base
content to hide. Commit replays both records in order: rename base /a → /b,
then remove base /b.

### Directory Rename Handling

R with dtype=d triggers a subtree move. All children follow the parent:

```
A(/dir, d, ino=1)       →  create /dir
A(/dir/f1, f, ino=2)    →  create /dir/f1
A(/dir/f2, f, ino=3)    →  create /dir/f2
R(/newdir, /dir, d)     →  detach /dir subtree, reattach at /newdir
```

Tree result:
```
/newdir   → A(d, ino=1)
/newdir/f1 → A(f, ino=2)
/newdir/f2 → A(f, ino=3)
```

Children's paths are correct by construction. No stale paths, no prefix
rewriting, no kernel re-emission of child records.

### Rename Chain Resolution

Chained renames resolve automatically through sequential processing:

```
R(/b, src=/a, f)    →  move /a node to /b; Tombstone at /a (was in base)
R(/c, src=/b, f)    →  move /b node to /c; no Tombstone at /b (not in base)
```

No separate chain resolution step. The second R finds the node where the
first R left it and moves it again. Only base paths get Tombstones — `/b`
was never in base so moving away from it leaves nothing to hide.
For diff/restore, each node records its original base path (set when first
moved from a base-only position). Subsequent moves don't change it.

### Base-Only Rename

When a base-only file (no prior tree node) is renamed, R creates the node:

```
R(/b, /a, f)   →  no node at /a → create Link { base_path: /a, dtype: f };
                   detach, reattach at /b; place Tombstone at /a
                   (source had base content)
```

Tree result: Link at /b (base_path=/a), Tombstone at /a. Diff shows
"renamed /a → /b". Commit replays R as `rename(base/a, base/b)`.
Restore injects redirect at /b (pointing to base /a) and deletion at /a
(hiding base /a from reads).

## Operations

### Diff / Status

Build the full dir tree, then walk it. Each dirent maps to a display label:

- Inode { in_base: false } → **added**
- Inode { in_base: true } → **modified** (diff `inodes/{ino}` vs base file)
- Tombstone → **deleted**
- Link → **renamed** (display `base_path → path`)

For `--at checkpoint` or `--from A --to B`, build two trees (before and after
the range) and compare. Nodes present only in the after-tree are new; nodes
only in the before-tree were removed; nodes in both with different dirents
were changed.

**Limitation:** a modified-then-renamed file (M then R) shows as "added" at the
new path and "deleted" at the old path — the rename relationship is lost. The
tree captures state, not history.

**Future work — rename-aware diff:** track rename provenance in the tree
(e.g. annotate moved nodes with their source path) so diff can display
"renamed and modified" instead of separate add + delete.

### Commit

Replay live records in journal order directly on the base filesystem.
First, parse markers and filter dead segments (same liveness algorithm as
current design). Then apply each live record one by one:

- A(path, dtype, ino) → for files/symlinks: copy `inodes/{ino}` to base `path` (create parents as needed). For directories: `mkdir(base/path)`.
- M(path, dtype, ino) → copy `inodes/{ino}` to base `path` (overwrite). Directories are never M (they have no staged content to modify).
- D(path, dtype) → remove base `path` (unlink for files/symlinks, rmdir for directories)
- R(dst, src, dtype) → `rename(base/src, base/dst)`. Destination must not exist.
- P(dst, src, dtype) → `rename(base/src, base/dst)`. POSIX rename overwrites the existing file at `base/dst`. Both are the same syscall; the R/P distinction exists for the tree builder, not for commit.

Each record is applied one by one. The base filesystem state after each
record matches what the overlay saw at the time of the next record — no
reordering, batching, or cycle detection needed. This relies on the kernel
emitting records in strict VFS operation order, serialized by inode
locks — parent directory operations are always recorded before child
operations. Directory renames with
staged children work naturally: the child's A/M record is applied first
(at the old path), then R moves the directory (children follow).

After replay: truncate journal, wipe `inodes/`, reset kernel via ioctl.

### Restore

1. Build dir tree from live segments up to checkpoint N
2. Walk tree — pass dirents directly to kernel via `ioctl(RESTORE)`. Each
   dirent maps 1:1 to a kernel `agfs_dirent`: Inode carries `ino` and
   `in_base`, Link carries `base_path`, Tombstone carries `ino=0`. No
   translation needed — the dir tree IS the dirent table.
3. Kernel clears dirent table, installs the received dirents, bumps gen to
   `new_gen`
4. Kernel writes `T\0<new_gen>\0<target_gen>\n`
5. Segments between `target_gen` and `new_gen` are dead

**Tree state → restore entry mapping:**

| Tree state | ino | in_base | Notes |
|-----------|-----|---------|-------|
| Inode(ino, dtype, in_base) | ino | in_base | Staged content |
| Link(base_path, dtype, in_base) | INO_REDIRECT, base=base_path | in_base | Linked to base_path |
| Tombstone | 0 (delete sentinel) | true | Deleted base path |
| none | — | — | Skip (intermediate directory, not part of dir tree) |

`in_base` is set by the journal record tag (A/R → false, M/P → true) and
determines whether a Tombstone is needed when the node vacates a position.

**Liveness filtering:** right-to-left walk of T markers. Each
`T(new_gen, target_gen)` kills segments between `target_gen` and `new_gen`.
Same algorithm as current design.

**Future work — diff-based restore:** Compute diff between current state and
target, send only changed entries via ioctl.

**Future work — tree cache:** Cache the serialized tree at each checkpoint
(e.g. `.agfs/cache/{gen}.tree`). Subsequent reads load the nearest cached tree
instead of replaying from scratch. Cache is invalidated by restore (which
changes liveness). Bounds read cost to O(|cached tree| + |segments since cache|).

## Kernel Writes

### What the kernel writes per VFS operation

| VFS operation            | Journal records              |
|--------------------------|------------------------------|
| create / mkdir / symlink | A (one record)               |
| write (COW)              | M (one record)               |
| unlink / rmdir           | D (one record)               |
| rename(old, new) — dest not in base | R (one record)  |
| rename(old, new) — dest in base     | P (one record)  |
| ioctl(CHECKPOINT)        | K(gen, name)                 |
| ioctl(RESTORE)           | T(new_gen, target_gen)       |

All renames — staged or redirect, file or directory — emit a single R or P
record. R if the destination is new; P if it overwrites a base path. The
tree builder handles the rest.

### Determining `src` (R/P records)

The kernel writes the **overlay path before the rename** — the dentry's current
path at rename time (`dentry_path_raw`). This is the immediate source, not the
resolved base path.

For chained renames (`mv a b`, then `mv b c`), the second rename writes
`src = /b`. The tree builder resolves this by moving /b (which was
already moved from /a) to /c.

## Key Changes from Current Design

| Aspect | Current | New |
|--------|---------|-----|
| Record types | 7 (A/M/D/R/P/K/S) | 7 (5 data + 2 markers: A/M/D/R/P/K/T) |
| Path format | (dir, name) pair | Full path |
| Rename entries | Staged: D+A/M. Redirect: R/P | All renames: single R or P |
| Resolution | Compaction pipeline (decompose, cancel, merge, ~700 lines) | Tree builder (insert, delete, move) |
| Directory renames | Stale child paths (bug) | Subtree move (correct by construction) |
| Chain resolution | Userspace multi-pass (compact.rs) | Sequential tree moves (automatic) |
| Per-segment diff | Requires comparing two states | Self-contained (A/M/D tags + tree for R/P src) |

## What This Eliminates

- `compact.rs` — decompose / cancel / merge passes (~700 lines)
- `action.rs` — Action type, apply(), collapse() logic
- The `Action` intermediate type (tree replaces it)

## Kernel Changes

- **`agfs_dirent.overwrites`** renamed to **`in_base`**. Semantics unchanged —
  the kernel still tracks whether the path existed in base to determine A vs M
  journal tag. The field name now matches the `Dirent::Inode { in_base }` type
  in userspace.
- **`agfs_dirent` states** renamed to match userspace terminology:
  - `ino > 0` → **Inode** (was "staged")
  - `ino = AGFS_INO_REDIRECT` → **Link** (was "redirect")
  - `ino = AGFS_INO_DELETED` → **Tombstone** (was "deleted")
- **Journal tags**: A/M replace ADD/MOD. D replaces DEL. R/P keep the same
  letters but now apply to all renames (staged + redirect), not just redirects.
  K stays. S renamed to T (avoid ambiguity with "staging").
- **Path format**: full overlay path instead of `(dir, name)` pair. The kernel
  writes `dentry_path_raw()` directly — no separate dir/name fields.
- **Rename records**: all renames (staged, redirect, file, directory) emit a
  single R or P record. The current split (staged → D+A/M, redirect → R/P) is
  removed.

## Test Changes

- **Deleted:** unit tests for `compact.rs` and `action.rs` (modules removed)
- **New:** unit tests for tree builder (insert, delete, move, subtree move)
- **New:** unit tests for rename chain resolution via tree
- **New:** unit tests for directory rename with children
- **Updated:** `tests/internals/` — journal format assertions change (new record
  tags, full paths)
- **Updated:** `tests/fs/` and `tests/cli/` — behavioral tests should mostly
  pass unchanged since user-visible semantics (status, diff, commit, restore)
  are preserved, but output format changes may require adjustments

## Documentation Updates

- **`docs/internals.md`** — rename S marker to T; update restore record format
  and examples; update record type list
- **`docs/architecture.md`** — remove RDR/REP references; update record tag
  descriptions to A/M/D/R/P; update path format (full paths, not dir+name pairs)
- **`docs/staging.md`** — update journal record type table and examples;
  remove the staged-vs-redirect rename split; update rename examples
- **`docs/permissions.md`** — update any references to journal record types
- **`docs/cli.md`** — update diff/status/commit output examples if format changes
