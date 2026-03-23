# 31 — Simplify dstate to tag + in_base

## Problem

The dstate packs a lot of information into a u64 — d_type, in_base, ino,
gen, and a kstrdup'd pointer — but most of it is redundant with other
per-dentry or per-inode state:

| Field | Stored in dstate | Derivable from |
|-------|-----------------|----------------|
| d_type (3 bits) | ✅ | `fs_umode_to_dtype(d_inode->i_mode)` |
| pointer (base_path) | ✅ | `lower_path.dentry` |
| ino (32 bits) | ✅ | `lower_path.dentry->d_name` (or not needed at all) |
| gen (16 bits) | ✅ | move to `agfs_inode_info` (per-inode, not per-dentry) |
| **tag (2 bits)** | ✅ | **essential** |
| **in_base (1 bit)** | ✅ | **essential** |

### d_type is derivable

All non-tombstone variants have `d_inode != NULL`, so
`fs_umode_to_dtype(d_inode->i_mode)` works.  Tombstones store d_type
but nobody reads it back — the CLI gets d_type from journal replay.

### base_path pointer is derivable

The kstrdup'd path string encodes the same location as
`lower_path.dentry`.  `dentry_path_raw(lower_dentry)` recovers the
string when needed (rename chain, roundtrip detection).

### ino is derivable (or unnecessary)

The inode store ID is only read back for:

1. **`agfs_open_staged_ino`** — looks up `.agfs/inodes/<ino>` by name.
   But `lower_path` already points to this file.  Use
   `dentry_open(&lower_path, ...)` directly.

2. **Rename** — reads ino to preserve it in the updated dstate.  Not
   needed if ino isn't in the dstate.

Journal records get ino from local variables at creation/COW time.

### gen belongs on the inode

Gen records when a file was last staged/COW'd.  This is an inode
property ("is this inode's data current?"), not a directory entry
property.  The agfs VFS inode (`d_inode(dentry)`) persists across COW —
only `lower_path` is swapped.  Adding `staging_gen` to
`agfs_inode_info` is natural.

## Approach

Replace the packed u64 dstate with a simple enum:

```c
enum agfs_dkind {
    AGFS_DKIND_PASSTHROUGH  = 0,
    AGFS_DKIND_STAGED_INODE = 1,
    AGFS_DKIND_REDIRECT     = 2,
    AGFS_DKIND_TOMBSTONE    = 3,
};

struct agfs_dentry_info {
    ...
    enum agfs_dkind         kind;
    bool                    in_base;
    ...
};
```

This matches the CLI's `Dstate` enum (`Passthrough`, `StagedInode`,
`BasePath`, `Tombstone`) with `in_base` as a separate field.  The CLI
should rename the variant `BasePath` → `Redirect`; the variant's
fields (`src`, `dtype`, `in_base`) stay the same since they are needed
for journal serialization and the wire format.

### State transition rules

`kind` and `in_base` change independently.

**kind** — determined by the operation:

| Before (kind) | Action | After (kind) |
|---------------|--------|--------------|
| any | Add / Mod | StagedInode |
| any (in_base) | Delete, Rename/Replace away | Tombstone |
| any (!in_base) | Delete, Rename/Replace away | Passthrough |
| StagedInode | Rename/Replace to | StagedInode |
| !StagedInode | Rename/Replace to | Redirect |
| Redirect | Replace roundtrip | Passthrough |

The journal record type (Add vs Mod, Rename vs Replace) is derived
from in_base — no separate distinction needed in the kind rules.

**in_base** — Add vs Mod and Rename vs Replace encode whether the
destination path has base content:

| Action | dst in_base |
|--------|------------|
| Add, Rename | false |
| Mod, Replace | true |

And move gen to the inode:

```c
struct agfs_inode_info {
    ...
    u16                 staging_gen; /* generation when last staged/COW'd */
    ...
};
```

### 1. Dstate simplification

Replace `struct agfs_dstate { u64 val; }` with `enum agfs_dkind`.
Remove all bit-packing, pointer embedding, dtype encoding.

Query functions simplify to trivial field reads.  Remove
`agfs_dentry_is_passthrough`, `agfs_dentry_is_base_path`,
`agfs_dentry_is_staged_inode`, `agfs_dentry_is_tombstone`, and
`agfs_dentry_in_base` — callers read `AGFS_D(d)->kind` and
`AGFS_D(d)->in_base` directly.  Lookup sets `in_base = true` for
positive base results, so the field is always valid.

### 2. Move gen to agfs_inode_info

Add `u16 staging_gen` to `agfs_inode_info`.  Reads and writes are
naturally atomic for `u16` on all kernel architectures; concurrent
access is serialized by `i_rwsem` (held during create, COW, and open).

Update:

- **`agfs_create_staged`** — set `AGFS_I(inode)->staging_gen = sbi->gen`
- **`agfs_do_cow`** — set `AGFS_I(inode)->staging_gen = sbi->gen`
- **`agfs_open_staged`** — check `AGFS_I(inode)->staging_gen >= sbi->gen`
- **`agfs_rename`** — no gen update needed (inode data unchanged)
- **Restore inject** — set after `agfs_iget`

### 3. Open staged files via lower_path

Replace `agfs_open_staged_ino(sbi, ino, flags)` with opening directly
from `lower_path`.  Keep the existing `O_TRUNC` handling
(`vfs_truncate` before `dentry_open`) and `staging_fd_count` error-path
accounting.  Eliminates the ino → path name lookup entirely.

### 4. Rename: derive base_src from lower_path

Replace `agfs_dentry_base_src(old_dentry)` with:

```c
dentry_path_raw(agfs_lower_dentry(old_dentry), buf, sizeof(buf))
```

Unifies redirect and passthrough cases — both derive the redirect source
from the lower dentry.

### 5. Mutations simplify

```c
void agfs_dentry_set_staged(struct dentry *d, bool in_base);
void agfs_dentry_set_redirect(struct dentry *d, bool in_base);
void agfs_dentry_unstage(struct dentry *d);  /* → passthrough */
```

No ino, gen, d_type, or base_copy parameters.  The caller is
responsible for setting `lower_path` (via `agfs_set_lower_path`)
*before* calling `agfs_dentry_set_staged` — this is already the case
today for create and COW, which resolve the inode store path before
updating dstate.

### 6. Restore inject

`agfs_dentry_add_staged_inode` — no gen parameter (set on inode after
`agfs_iget`).

`agfs_dentry_inject_inode` — resolves lower_path via ino, sets
kind/in_base, sets `staging_gen` on the inode, and adds to dcache.

`agfs_dentry_inject_redirect` — resolves lower_path via
`kern_path(base_path)`, sets kind/in_base, and adds to dcache.
Takes ownership of the base_path string.

Wire format changed from `u64` to a compact `kind:u8` + per-kind
fields.  Each variant is self-describing:
- Passthrough (0): no extra data.
- StagedInode (1): `ino:le32` + `in_base:u8`.
- Redirect (2): `base_len:le16` + `base:u8[base_len]` + `in_base:u8`.
- Tombstone (3): no extra data.

### 7. Tombstone d_type

Remove d_type from tombstone encoding.  The journal delete record
already carries d_type (from the caller, derived from `d_inode->i_mode`
before deletion).  `agfs_dentry_add_tombstone` no longer needs a
d_type parameter — tombstones are just negative dentries with
`kind = AGFS_DKIND_TOMBSTONE`.

Wire format: the d_type in the tombstone's serialized u64 is still
read during restore but ignored (not stored in the dstate).

## What's removed

- `struct agfs_dstate { u64 val; }` — replaced by `enum agfs_dkind`
- All bit-packing/unpacking (dtype_pack, dtype_unpack, pointer recovery)
- `agfs_dstate_free` — no embedded pointers to free
- `kstrdup` in rename path — no string allocation
- `agfs_open_staged_ino` — open via lower_path instead
- `agfs_dentry_ino`, `agfs_dentry_gen`,
  `agfs_dentry_base_src` — all removed
- `agfs_dentry_is_passthrough`, `agfs_dentry_is_staged_inode`,
  `agfs_dentry_is_base_path`, `agfs_dentry_is_tombstone`,
  `agfs_dentry_in_base` — callers read fields directly
- `agfs_dentry_is_current` — replaced by `agfs_dentry_is_current()` inline using inode gen

## Files affected

| File | Change |
|------|--------|
| `agfs.h` | Replace `struct agfs_dstate` with `enum agfs_dkind`; add `in_base` to dentry_info; add `staging_gen` to inode_info; simplify query inlines |
| `dentry.c` | Remove all encoding internals; simplify mutations; update inject helpers |
| `inode.c` | Simplify create/delete/rename; set staging_gen on inode |
| `staging.c` | Simplify COW; set staging_gen on inode; open via lower_path |
| `file.c` | Check inode staging_gen; open via lower_path |
| `dir.c` | Already uses `d_inode` for readdir (no change) |
| `ioctl.c` | Update wire decode; set staging_gen after agfs_iget |
