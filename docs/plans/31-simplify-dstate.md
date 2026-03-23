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
enum agfs_dentry_kind {
    AGFS_DENTRY_PASSTHROUGH  = 0,
    AGFS_DENTRY_STAGED_INODE       = 1,
    AGFS_DENTRY_REDIRECT     = 2,
    AGFS_DENTRY_TOMBSTONE    = 3,
};

struct agfs_dentry_info {
    ...
    enum agfs_dentry_kind   kind;
    bool                    in_base;
    ...
};
```

This matches the CLI's `Dstate` enum (`Passthrough`, `StagedInode`,
`BasePath`, `Tombstone`) with `in_base` as a separate field.  The CLI
should rename `BasePath` → `Redirect`.

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

Replace `struct agfs_dstate { u64 val; }` with `enum agfs_dentry_kind`.
Remove all bit-packing, pointer embedding, dtype encoding.

Query functions simplify to trivial field reads:

```c
static inline bool agfs_dentry_is_passthrough(const struct dentry *d)
{
    return AGFS_D(d)->kind == AGFS_DENTRY_PASSTHROUGH;
}

static inline bool agfs_dentry_in_base(const struct dentry *d)
{
    if (AGFS_D(d)->kind == AGFS_DENTRY_PASSTHROUGH)
        return d_inode(d) != NULL;
    return AGFS_D(d)->in_base;
}
```

### 2. Move gen to agfs_inode_info

Add `u16 staging_gen` to `agfs_inode_info`.  Update:

- **`agfs_create_staged`** — set `AGFS_I(inode)->staging_gen = sbi->gen`
- **`agfs_do_cow`** — set `AGFS_I(inode)->staging_gen = sbi->gen`
- **`agfs_open_staged`** — check `AGFS_I(inode)->staging_gen >= sbi->gen`
- **`agfs_rename`** — no gen update needed (inode data unchanged)
- **Restore inject** — set after `agfs_iget`

### 3. Open staged files via lower_path

Replace `agfs_open_staged_ino(sbi, ino, flags)` with opening directly
from `lower_path`:

```c
agfs_get_lower_path(dentry, &lower_path);
f = dentry_open(&lower_path, flags, current_cred());
agfs_put_lower_path(dentry, &lower_path);
```

Eliminates the ino → path name lookup entirely.

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

No ino, gen, d_type, or base_copy parameters.

### 6. Restore inject

`agfs_dentry_add_staged_inode` — no gen parameter (set on inode after
`agfs_iget`).

`agfs_dentry_add_redirect` — accepts path string for `kern_path` to set
lower_path, then frees it.  No pointer embedding.

Wire format unchanged — still carries the u64 and trailing base_path
string.  The decode extracts ino + in_base + kind; ino is only used for
`agfs_inode_path` during lower_path resolution.

### 7. Tombstone d_type

Remove d_type from tombstone encoding.  The journal delete record
already carries d_type (from the caller, derived from `d_inode->i_mode`
before deletion).  `agfs_dentry_add_tombstone` no longer needs a
d_type parameter — tombstones are just negative dentries with
`dstate = AGFS_DENTRY_TOMBSTONE`.

Wire format: the d_type in the tombstone's serialized u64 is still
read during restore but only passed to the journal (if needed), not
stored in the dstate.

## What's removed

- `struct agfs_dstate { u64 val; }` — replaced by `enum agfs_dentry_kind`
- All bit-packing/unpacking (dtype_pack, dtype_unpack, pointer recovery)
- `agfs_dstate_free` — no embedded pointers to free
- `kstrdup` in rename path — no string allocation
- `agfs_open_staged_ino` — open via lower_path instead
- `agfs_dentry_ino`, `agfs_dentry_gen`,
  `agfs_dentry_base_src` — all removed
- `agfs_dentry_is_current` — replaced by inode gen check

## Files affected

| File | Change |
|------|--------|
| `agfs.h` | Replace `struct agfs_dstate` with `enum agfs_dentry_kind`; add `in_base` to dentry_info; add `staging_gen` to inode_info; simplify query inlines |
| `dentry.c` | Remove all encoding internals; simplify mutations; update inject helpers |
| `inode.c` | Simplify create/delete/rename; set staging_gen on inode |
| `staging.c` | Simplify COW; set staging_gen on inode; open via lower_path |
| `file.c` | Check inode staging_gen; open via lower_path |
| `dir.c` | Already uses `d_inode` for readdir (no change) |
| `ioctl.c` | Update wire decode; set staging_gen after agfs_iget |
