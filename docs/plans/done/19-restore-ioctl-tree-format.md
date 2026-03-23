# 19 — Restore ioctl: consume DirTree directly

## Problem

The restore ioctl consumes a flat array of `agfs_ioc_restore_entry` structs, each
carrying a full absolute path plus dirent metadata.  The CLI builds a `DirTree`,
flattens it to `Vec<(String, Dirent)>`, converts each entry into a
`AgfsIocRestoreEntry` with raw pointers, and sends the array to the kernel.  The
kernel then re-resolves every path from the root via `vfs_path_lookup` to find
each entry's parent directory.

This is wasteful:

* Full paths are sent for every entry — shared directory prefixes are duplicated.
* The kernel re-resolves every path from scratch, doing repeated VFS lookups
  through the same directory chain.
* The CLI performs two unnecessary intermediate allocations (`to_dirents` +
  `dirents_to_entries`) between the natural tree representation and the wire
  format.
* `split_parent_child` + `vfs_path_lookup` in the kernel is glue code that only
  exists because the wire format lost the tree structure.

## Approach

Replace the flat entry array with a serialized DirTree.  The CLI serializes the
`DirTree` depth-first into a contiguous byte buffer and sends it as a single
blob.  The kernel `vmalloc`s + `copy_from_user`s the buffer in one call, then
walks it iteratively with an explicit directory stack — no `vfs_path_lookup`, no
`split_parent_child`, no per-entry `copy_from_user`.

### Wire format

All multi-byte integers are **little-endian** (native x86).

```
TreeBuf      := NodeList
NodeList     := child_count:le16  Node[child_count]
Node         := name_len:le16  name:u8[name_len]
                Dstate
                child_count:le16  Node[child_count]
Dstate       := val:le64                                    (0 = Passthrough)
                [base_len:le16  base_path:u8[base_len]  if link]
```

Every node has two independent, optional parts: a dstate and a child list.
No tag switch — the kernel checks `val` and `child_count` independently.

Mapping from `DirNode`:

| Rust type                            | val      | child_count |
|--------------------------------------|----------|-------------|
| `DirNode::File(dirent)`              | non-zero | 0           |
| `DirNode::Dir(Passthrough, subtree)` | 0        | N           |
| `DirNode::Dir(dirent, subtree)`      | non-zero | N           |

#### Packed dstate encoding

The wire `val` u64 mirrors the kernel's `struct agfs_dstate` bit layout, with gen /
pointer bits zeroed:

| State       | val (le64)                                                       | Trailing data                     |
|-------------|-------------------------------------------------------------------|-----------------------------------|
| Passthrough | `0`                                                               | —                                 |
| Tombstone   | `(s64)val > 0, ino == 0`: `[62:60]=dtype [59]=1 [58:0]=0`        | —                                 |
| Inode       | `[63]=0  [62:60]=dtype  [59]=in_base  [58:16]=0  [47:16]=ino  [15:0]=0` | —                                 |
| Link        | `[63]=1  [62:60]=dtype  [59]=in_base  [58:0]=0`                  | `base_len:le16  base_path:bytes`  |

The kernel distinguishes the states with the same checks as `struct agfs_dstate`:

* `val == 0` → passthrough — skip dentry creation, proceed to child_count.
* `(s64)val > 0 && ino == 0` → tombstone — use as-is.
* `(s64)val > 0 && ino != 0` → inode — stamp gen: `val = (wire & ~0xFFFF) | new_gen`.
* `(s64)val < 0` → link — read trailing `base_len` + `base_path`,
  call `agfs_dstate_base_path(buf, dt, ib)`.  The kernel uses `kstrndup` to
  NUL-terminate the copy.

Gen bits `[15:0]` are zeroed on the wire because the kernel assigns `new_gen`.
Pointer bits `[58:0]` are zeroed for links because the base path travels inline
instead of as a kernel pointer.

This eliminates `AGFS_INO_REDIRECT` and `AGFS_INO_DELETED` from the wire
protocol entirely.  `AGFS_INO_REDIRECT` stays in the kernel only for
`agfs_dstate_emit_ino()` (readdir).  `AGFS_INO_DELETED` and CLI `INO_REDIRECT`
become dead code and are removed.

Children within each `DirTree` level are sorted by name for deterministic
output (useful for testing and debugging; the kernel doesn't require ordering).

### Ioctl struct change

```c
/* Before */
struct agfs_ioc_restore {
    __u64 target_gen;
    __u64 new_gen;
    __u64 entry_count;   /* ← removed */
    __u64 entries_ptr;   /* ← removed */
};

/* After */
struct agfs_ioc_restore {
    __u64 target_gen;    /* in: checkpoint gen (0 = reset) */
    __u64 new_gen;       /* out: new generation assigned */
    __u64 tree_len;      /* in: byte length of serialized tree */
    __u64 tree_ptr;      /* in: userspace pointer to tree buffer */
};
```

`struct agfs_ioc_restore_entry` is deleted entirely.

Ioctl number stays `_IOWR('A', 41, ...)` — the header struct size is unchanged
(32 bytes).

### Kernel-side injection

Replace `agfs_restore_inject` (flat loop with `vfs_path_lookup` per entry) with
an iterative tree walker.

**Buffer handling**: `vmalloc` + `copy_from_user` the whole buffer up front.
Return `-ENOMEM` if `vmalloc` fails.  Cap at `AGFS_RESTORE_MAX_TREE_LEN`
(e.g. 16 MiB) to avoid unbounded allocation.  `vfree` after injection completes
(success or failure).

**Tree walker pseudocode**:

```
dir_stack[0] = { dentry = dget(sb->s_root), remaining = root.child_count }

while depth >= 0:
    if stack[depth].remaining == 0:
        dput(stack[depth].dentry)
        depth--
        continue
    stack[depth].remaining--

    read name_len, name from cursor
    dir = d_inode(stack[depth].dentry)

    read dstate val from cursor
    if val != 0:
        parse val → dstate
        inode_lock(dir)
        agfs_add_dirent(dir, name, name_len, val)
        inode_unlock(dir)

    read child_count from cursor
    if child_count > 0:
        child = lookup_one_len(name, stack[depth].dentry, name_len)
        depth++
        stack[depth] = { dentry = child, remaining = child_count }
```

Max depth capped at `AGFS_RESTORE_MAX_DEPTH` (64).

**Validation** — the kernel must check at every read:

* Cursor doesn't advance past `buf + tree_len` (return `-EINVAL`).
* `name_len > 0` and `name_len <= NAME_MAX` (255).
* `val` inode: `ino != 0`, and
  dtype bits `[62:60]` are a valid 3-bit packed d_type.
* `val` link: dtype bits `[62:60]` are valid, `base_len > 0`,
  `base_len < AGFS_PATH_MAX`, and trailing byte is `'\0'`.
* Node must have `val != 0 || child_count > 0` (reject empty nodes).
* `depth < AGFS_RESTORE_MAX_DEPTH` before pushing.
* `lookup_one_len` succeeds (return its error otherwise).

After the walk completes successfully, verify `cursor == buf + tree_len`
(trailing data means a malformed buffer — return `-EINVAL`).

On any error the walker breaks, the cleanup loop `dput`s all stacked dentries,
and the partially-injected state is left for the CLI to retry or abort (same
semantics as the current flat injector).

**Dead code removal**: `split_parent_child` is only used by the restore
injector — remove it.  `agfs_copy_user_path` is shared with rule and checkpoint
ioctls — keep it.

### CLI-side serialization

Add `DirTree::serialize(&self) -> Vec<u8>` which performs a depth-first walk,
writing each node into a byte buffer per the wire format above.  Delete
`dirents_to_entries()` and `AgfsIocRestoreEntry`.

The restore command becomes:

```rust
let tree = journal.into_tree_at(target_gen);
let count = tree.len();
let buf = tree.serialize();
let _new_gen = ioctl::restore(&ctl_file, target_gen, &buf)?;
```

Abort path (`target_gen=0`) passes an empty buffer (`tree_len=0, tree_ptr=0`).

## Todos

1. **docs-update** — Update `docs/internals.md` and `docs/staging.md` to
   describe the new wire format and tree-walking injection.  Remove references
   to `agfs_ioc_restore_entry` and the flat entry array.

2. **cli-serialize** — Add `DirTree::serialize(&self) -> Vec<u8>` in
   `cli/journal/tree.rs`.  Internally use a `serialize_into(&self, buf)` helper
   so the recursive DirTree levels share the same buffer.  Each node writes
   `name_len + name + val:le64 + [base_len + base_path if link] + child_count + children`.
   Serialize each `Dirent` as a u64 val mirroring `struct agfs_dstate` (gen/pointer
   bits zeroed), with trailing `base_len + base_path` for links.  Add
   `DType::to_packed()` in `types.rs` returning the 2-bit encoding (File→0,
   Dir→1, Link→2) matching the kernel's `agfs_dtype_pack`.  Validate
   `ino ≤ 0xFFFFFFFFFFF` (44-bit range) during serialization.  Skip
   `DirNode::Dir(None, empty_subtree)` nodes (they carry no information).
   Add unit tests:
   - Empty tree → `[0x00, 0x00]` (child_count = 0).
   - Single file (Inode, Link, Tombstone variants) — verify dstate val bits.
   - Nested directories (passthrough + with dirent).
   - Children are sorted by name.
   - Link has trailing base_path; Inode and Tombstone do not.
   - Passthrough dir with empty subtree is omitted from output.

3. **cli-ioctl** — Update `cli/ioctl.rs`: change `AgfsIocRestore` fields to
   `tree_len`/`tree_ptr`, delete `AgfsIocRestoreEntry`, update `restore()` to
   accept `&[u8]`.  Update size assertion (struct stays 32 bytes).

4. **cli-restore-cmd** — Update `cli/cmd/restore.rs`: remove
   `dirents_to_entries()`, call `tree.serialize()` directly, use `tree.len()`
   for the output count.  Delete the three `dirents_to_entries_*` tests.  Keep
   all tree-building tests (they test `DirTree`, not the wire format).

5. **kmod-header** — Update `kmod/agfs.h`: delete `agfs_ioc_restore_entry`.
   Rename `agfs_ioc_restore` fields (`entry_count` → `tree_len`,
   `entries_ptr` → `tree_ptr`).  Delete `AGFS_INO_DELETED` (dead after this
   change).  Add constants: `AGFS_RESTORE_MAX_DEPTH` (64),
   `AGFS_RESTORE_MAX_TREE_LEN` (16 MiB).

6. **kmod-inject** — Rewrite `agfs_restore_inject` in `kmod/ioctl.c`:
   `vmalloc` + `copy_from_user` the buffer, walk iteratively with a cursor +
   explicit `dir_stack[AGFS_RESTORE_MAX_DEPTH]`.  Add cursor helpers
   (`read_u8`, `read_u16`, `read_u64`, `read_bytes`).  For each node: read
   `val` — if non-zero, parse dstate (`(s64)>0` →
   stamp gen, `(s64)<0` → read base_path + `agfs_dstate_link`); read
   `child_count` — if >0, `lookup_one_len` + push.  Remove
   `split_parent_child` (restore-only).

7. **cli-cleanup** — Remove `INO_REDIRECT` from `cli/journal/types.rs` (no
   longer used after `dirents_to_entries` is deleted).  Update `journal/mod.rs`
   comment.

8. **vm-test** — `make vm-build && make vm-test` to verify everything passes.

## Notes

* `to_dirents()` stays — it's used by ~40 tree.rs unit tests and 3
  integration tests (`tests/internals/helpers.rs`, `tests/fs/test_rename.rs`,
  `tests/internals/test_restore.rs`).  Removing it would be a large scope
  addition for no functional benefit.  After this change, `restore.rs` no
  longer calls `to_dirents` — its only remaining callers are tree.rs tests
  and the three integration tests above.
* `agfs_add_dirent` already `kstrdup`s link base paths from the dstate value,
  so passing pointers into the `vmalloc`'d tree buffer is safe — the buffer can
  be `vfree`'d after injection.
* `DirNode::Dir(Passthrough, subtree)` (pass-through directory, exists only
  because children were modified) serializes with `val=0` — the kernel
  looks up the existing base directory without injecting a dirent.
* `DirNode::Dir(Passthrough, empty_subtree)` is skipped by the serializer
  entirely — it carries no information (no own dirent, no children).
* `DirNode::Dir(dirent, empty_subtree)` (created directory with no children)
  serializes with `val!=0, child_count=0` — the dirent is injected but
  no stack push is needed.
* `Dstate::Tombstone` serializes with `d_type` and `in_base=1` encoded in the
  dstate value.  `val == 0` is now passthrough (accepted by the kernel, no dentry created).
* Reset mode (`target_gen=0`) skips injection entirely, so `tree_len=0,
  tree_ptr=0` works.
