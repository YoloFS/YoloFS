# 33 — Dentry state redesign: `target + in_base + structural shape`

## Problem

The old CLI dentry model used a 4-variant enum (`StagedInode`, `Redirect`,
`Tombstone`, `Unset`) that mixed together three different concerns:

1. where content lives,
2. whether the path existed in base before staging, and
3. whether the node is a file or a directory.

The kernel changes in this redesign already split the first two concerns into
`target` and `in_base`, with `pinned` handling the staged-vs-ground-state
question. The CLI side now follows the same separation and encodes
directory-vs-file shape structurally in the tree instead of storing `dtype`
inside every resolved dentry.

This document describes the design as implemented in the current tree.

## Implemented representation

### CLI (Rust)

```rust
pub struct Dentry {
    pub target: Target,
    pub in_base: bool,
}

pub enum Target {
    Inode(u32),            // staged inode in the flat inode store
    Path(Option<String>),  // Some(src) = redirect, None = passthrough scaffold
    None,                  // content absent
}

pub enum DirNode {
    File(Dentry),
    Dir(Dentry, DirTree),
}
```

Key points:

- `Dentry` carries only overlay state: **where content lives** and
  **whether the path existed in base**.
- `Dentry` does **not** carry `dtype`.
- Directory-vs-leaf shape is represented by `DirNode::Dir` vs `DirNode::File`.
- A passthrough/scaffold directory is represented as
  `DirNode::Dir(Dentry::passthrough(), subtree)`, where
  `Dentry::passthrough()` is `Target::Path(None)` with `in_base=true`.
- The journal still records `dtype`, but tree construction only uses it to
  decide whether an action creates a `DirNode::Dir` or `DirNode::File`.
  Once the tree is built, regular-file-vs-symlink information is recovered
  from the backing inode or base path when needed.

### Kernel (C)

```c
enum agfs_target {
    AGFS_TARGET_INODE = 1,
    AGFS_TARGET_PATH  = 2,
    AGFS_TARGET_NONE  = 3,
};

struct agfs_dentry_info {
    spinlock_t       lock;
    struct path      lower_path;
    enum agfs_target target;
    bool             in_base;
    bool             pinned;
    enum agfs_perm   perm;
    struct list_head rule_pin;
    struct dentry    *rule_dentry;
};
```

Key points:

- The kernel stores `target`, `in_base`, and `pinned`.
- The only ground/unpinned state is `(AGFS_TARGET_NONE, false)`.
- Passthrough is therefore **not** a target on the kernel side; it is simply
  the absence of staged state (`pinned=false`).
- File type is derived from the lower inode when lookup / readdir / restore
  attaches or resolves dentries.

## Mapping from old concepts

| Old concept | Current CLI representation | Current kernel representation |
|-------------|----------------------------|-------------------------------|
| `Unset` intermediate dir | `Dir(Dentry::passthrough(), subtree)` | no pinned state |
| `StagedInode { ino, in_base }` | `Dentry { target: Inode(ino), in_base }` | `target=INODE`, `in_base`, `pinned=true` |
| `Redirect { src, in_base }` | `Dentry { target: Path(Some(src)), in_base }` | `target=PATH`, `in_base`, `pinned=true` |
| `Tombstone` | `Dentry { target: None, in_base: true }` | `target=NONE`, `in_base=true`, `pinned=true` |
| cancelled / ground state | node removed or passthrough scaffold only | `target=NONE`, `in_base=false`, `pinned=false` |

## Restore wire format

Current wire format:

```text
DirTree := child_count:le16  DirNode[child_count]
DirNode := name_len:le16  name:u8[name_len]
           Dentry
           child_count:le16  DirNode[child_count]

Dentry  := tag:u8  in_base:u8  [payload]
           tag=1 Inode  => ino:le32
           tag=2 Path   => path_len:le16  path:u8[path_len]
           tag=3 None   => no payload
```

Important details:

- There is **no tag 0** in the implemented format.
- Passthrough/scaffold directories are encoded as `tag=2` (`PATH`) with
  `in_base=1` and `path_len=0`.
- Empty passthrough dirs are omitted from serialization.
- On restore, the kernel treats `PATH` + `path_len==0` as a passthrough
  no-op for staged state, then descends via normal dcache/base lookup when
  the node has children.

## Changes by area

### CLI — `cli/journal/dentry.rs`

- Replace the old 4-variant dentry enum with `struct Dentry` plus `enum Target`.
- Drop `dtype` from resolved dentries.
- Add `Dentry::passthrough()` and `is_passthrough()`.
- Keep `ino()` and `matches_path()` as target-based helpers.

### CLI — `cli/journal/tree.rs`

- Keep `DirNode::Dir(Dentry, DirTree)`; passthrough is represented by the
  dentry value, not by `Option<Dentry>`.
- Use journal `dtype` only while applying actions to decide whether to create
  a directory node or a file node.
- `get()` returns a dentry only for non-passthrough nodes.
- `get_node()` exposes the structural node shape when callers need to know
  directory-vs-file.
- Roundtrip collapse restores directory passthrough state with
  `Dentry::passthrough()` or removes file nodes entirely.

### CLI — serialization / restore

- Serialize `Target::Inode`, `Target::Path(Some(src))`, and `Target::None`
  directly.
- Serialize passthrough scaffolds as `Target::Path(None)` with empty path.
- Do not serialize empty passthrough dirs.

### CLI — `cli/cmd/diff.rs`, `cli/cmd/restore.rs`, tests

- Match on `target` and `in_base`, not legacy enum variants.
- Use `DirNode` shape when tests need to distinguish directories from leaves.
- Recover regular-file vs symlink behavior from the backing inode/path rather
  than expecting `dtype` on resolved CLI dentries.

### Kernel — `kmod/agfs.h`, `kmod/dentry.c`, `kmod/inode.c`, `kmod/ioctl.c`

- Replace `kind`/`agfs_dkind` with `target`/`agfs_target`.
- Introduce explicit `pinned` handling via `agfs_dentry_set()` /
  `agfs_dentry_reset()`.
- Restore path understands the recursive tree format and the empty-path
  passthrough encoding.

## Notes

- The representable `(Target::None, in_base=false)` state is the ground /
  cancelled state. It means “no staged overlay state here”.
- The CLI tree intentionally preserves only the information needed to
  reconstruct overlay state and restore it into the kernel. It does not try to
  retain every original journal field on each resolved dentry.
- File type still matters in the journal and in the kernel, but it is no
  longer part of the resolved CLI `Dentry` value.
