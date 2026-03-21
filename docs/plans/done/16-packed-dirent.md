# Plan: Pack `agfs_dirent` fields into a single `u64`

## Problem

`agfs_dirent` currently stores five separate fields (`ino`, `base`, `gen`,
`d_type`, `in_base`) using 32 bytes (u64 + ptr + u64 + u8 + bool + padding).
These can be packed into a single `u64 packed` field, saving ~20 bytes per
dirent (from 48 → 28 bytes fixed overhead before `name[]`).

## Naming

Align kernel terminology with the CLI's `Dirent` enum:

| CLI          | Kernel (old)     | Kernel (new)     |
|--------------|------------------|------------------|
| `Inode`      | staged           | inode            |
| `Link`       | redirect         | link             |
| `Tombstone`  | deleted          | tombstone        |

Helper names follow: `agfs_de_inode(...)`, `agfs_de_link(...)`,
`agfs_de_is_inode()`, `agfs_de_is_link()`, `agfs_de_is_tombstone()`.
Tombstone is simply `packed == 0` — no constant needed.

## Cancelled-entry removal

Currently the kernel never removes dirent entries from the hash table.
A delete just sets `ino = 0` and preserves `in_base`. This leaves "cancelled"
entries (deleted with `in_base=false`, e.g. after A then D) lingering in the
hash table until the entire directory is torn down. These entries carry no
useful information — `old_de && old_de->in_base` produces the same result as
`NULL` for cancelled entries.

This refactoring removes cancelled entries: when `add_dirent` transitions an
entry to tombstone and `in_base` is false, `hlist_del` + `kfree` the entry
instead of keeping it. This means a surviving tombstone always has
`in_base=true`, matching the CLI's `Dirent::Tombstone` which hardcodes
`in_base=true`.

This is safe in lookup: `lookup.c` distinguishes `!de` (fall through to
base, return 0) from `de && tombstone` (negative dentry, return 1).  After
cancellation, an `in_base=false` tombstone becomes `!de`, hitting the
"fall through to base" path — which is correct because `in_base=false`
means no base entry exists, so the base lookup also returns nothing.

Benefits:
- Less memory and faster lookups/readdir (fewer dead entries)
- Tombstone becomes a single constant in the packed encoding (no `in_base` bit)
- Kernel and CLI state models become isomorphic

## `d_type` encoding

The libc `DT_*` constants (DT_REG=8, DT_DIR=4, DT_LNK=10) need 4 bits.
We compress to 2 bits with a private encoding, converting at
encode/decode boundaries. `11` is reserved as a bug sentinel.

| 2-bit value | Meaning  | Libc constant |
|-------------|----------|---------------|
| `00`        | regular  | `DT_REG` (8)  |
| `01`        | directory| `DT_DIR` (4)  |
| `10`        | symlink  | `DT_LNK` (10) |
| `11`        | invalid  | — (bug check) |

Conversion helpers: `agfs_dtype_pack(unsigned char libc_dt)` → 2-bit value,
`agfs_dtype_unpack(u64 packed_dt)` → libc value. Both are small switch/lookup.
`agfs_dtype_pack` should `WARN_ON_ONCE` and return `11` for unrecognised
input (e.g. `DT_UNKNOWN` from a buggy ioctl caller).
`agfs_dtype_unpack` should `WARN_ON_ONCE` and return `DT_UNKNOWN` if it
sees `11`.

## Encoding

Three mutually exclusive states discriminated by **bit 0** and zero:

- `packed == 0` → tombstone
- `packed & 1 == 1` → link (odd — pointer with bit 0 set)
- `packed != 0 && !(packed & 1)` → inode (even, non-zero — ino > 0 guarantees this)

`d_type` (bits [63:62]) and `in_base` (bit [61]) are at the same position
in inode and link, making all common accessors branchless.

### Tombstone (value = 0)

Always `in_base=true`. A delete of an `in_base=false` entry removes the
entry entirely (cancellation) rather than creating a tombstone.

Tombstone carries no `d_type` — no kernel code reads `d_type` from
tombstones (readdir skips them, rename rejects them). The CLI sends
`d_type` during restore but the kernel ignores it for tombstones.

`agfs_del_dirent` stays as the clean `&(struct agfs_dirent){0}` (zero-init).

`agfs_read_dirent` returns 0 when no dirent is found. Callers treat both
"no entry" and "tombstone" identically: `is_inode` → false, fall through
to the slow path. This is safe because VFS lookup prevents opening a
tombstoned file.

### Inode (even, non-zero)

```
[63:62]  d_type    2 bits  (private encoding, see above)
[61]     in_base   1 bit
[60:16]  ino      45 bits  (max ~35.2 trillion; always > 0)
[15:1]   gen      15 bits  (max 32767)
[0]      0
```

`agfs_de_inode` must `WARN_ON_ONCE` if `ino` exceeds 45 bits, `ino == 0`,
or `gen` exceeds 15 bits — these indicate bugs in the caller.  The gen
comparison in `agfs_open_staged` must mask `sbi->gen` to 15 bits so it
matches the truncated stored value.

### Link (odd)

The `kstrdup` pointer (≥ 8-byte aligned, so bit [0] = 0) is stored with
bit [0] flipped to 1 as the tag. `d_type` and `in_base` borrow bits
[63:61] from kernel sign-extension (safe on x86_64 4- and 5-level
paging — at least bits [63:57] are sign extension for kernel addresses).

Add `BUILD_BUG_ON(ARCH_KMALLOC_MINALIGN < 8)` to validate the alignment
assumption.  Add `BUILD_BUG_ON(!IS_ENABLED(CONFIG_X86_64))` since the
link encoding relies on x86_64 canonical-form sign extension.

`agfs_de_link` must `WARN_ON_ONCE` if the pointer's bits [63:57] are not
all 1s (validating the sign-extension assumption at runtime, not just the
alignment `BUILD_BUG_ON`).

```
[63:62]  d_type    2 bits  (borrowed from sign extension)
[61]     in_base   1 bit   (borrowed from sign extension)
[60:1]   pointer bits [60:1]  (real address bits)
[0]      1                    (tag — pointer bit 0 was 0)
```

Pointer recovery: `(packed & 0x1FFFFFFFFFFFFFFE) | 0xE000000000000000`
(restore sign-extension bits [63:61] = 111, clear tag bit [0]).

### Branchless accessors

```c
is_tombstone = !packed
is_link      = packed & 1
is_inode     = packed && !(packed & 1)
d_type       = agfs_dtype_unpack((packed >> 62) & 3)  // inode + link
in_base      = (packed >> 61) & 1                      // inode + link
ino          = (packed >> 16) & 0x1FFFFFFFFFFF         // inode only (45 bits)
gen          = (packed >> 1) & 0x7FFF                  // inode only
base         = (packed & 0x1FFFFFFFFFFFFFFE)
             | 0xE000000000000000                      // link only
```

### `dir_emit` inode number

`dir_emit` needs a VFS ino for all non-tombstone entries. `agfs_de_ino()`
only works for inode entries. Add `agfs_de_emit_ino(packed)`: returns the
real ino for inode entries, `(u64)-1` for links (preserving the current
`AGFS_INO_REDIRECT` behavior so readdir output is unchanged).

## Resulting struct layout

```c
struct agfs_dirent {
	struct hlist_node	node;       /* 16 bytes */
	u64			packed;     /*  8 bytes */
	unsigned int		name_len;   /*  4 bytes */
	char			name[];     /*  flexible */
};  /* 28 bytes fixed overhead before name[] */
```

## Call-site changes

### `agfs_add_dirent` (staging.c)

When transitioning to tombstone (`de->packed == 0`):
- If existing entry has `in_base=true`: free any link pointer, set
  `packed = 0`.
- If existing entry has `in_base=false`: free link pointer, `hlist_del`,
  `kfree` the entry (cancellation).
- If no existing entry (deleting a base-only file): allocate new entry with
  `packed = 0`.

### `agfs_del_dirent` (staging.c)

Zero-init naturally produces tombstone: `agfs_add_dirent(dir, name, len, &(struct agfs_dirent){0})`.

### `agfs_free_de_buckets_locked` (staging.c)

Replace `kfree(de->base)` with `agfs_de_free_base(de->packed)`.

### `agfs_do_cow` (staging.c)

Replace field-by-field init with `agfs_de_inode(ino, gen, DT_REG, true)`.

### Lookup (lookup.c)

Replace `agfs_ino_is_staged(de->ino)` → `agfs_de_is_inode(de->packed)`,
`agfs_ino_is_redirect(de->ino)` → `agfs_de_is_link(de->packed)`.
Extract ino via `agfs_de_ino(de->packed)`, base via `agfs_de_base(de->packed)`.

### `agfs_read_dirent` / `agfs_emit_dirents` (file.c)

`agfs_read_dirent` returns a single `u64 packed` (replaces two out-params
`*ino, *gen`). Returns 0 when no dirent exists (= tombstone, safe).

In `agfs_open_staged`, the caller transformation:
- `agfs_ino_is_staged(ino)` → `agfs_de_is_inode(packed)`
- `ino` for `agfs_open_staged_ino()` → `agfs_de_ino(packed)`
- `gen` for COW check → `agfs_de_gen(packed)`

`agfs_emit_dirents`: skip tombstones via `agfs_de_is_tombstone(de->packed)`,
emit via `agfs_de_emit_ino(de->packed)` and `agfs_de_d_type(de->packed)`.

### Create / rename (inode.c)

- Create: `old_de->in_base` → `agfs_de_in_base(old_de->packed)`.
  Construct via `agfs_de_inode(ino, gen, dt, in_base)`.
- Rename: read src fields via decode helpers. Tombstone check at
  inode.c:154 (`agfs_ino_is_deleted(ino)`) → `agfs_de_is_tombstone(src_de->packed)`.
  Destination dirent via `agfs_de_inode(...)` or `agfs_de_link(...)`.

### Restore inject (ioctl.c)

Map UAPI `ent.ino` (0 / -1 / >0) to the packed encoding:
- `ent.ino == 0` → `0` (tombstone)
- `ent.ino == AGFS_INO_REDIRECT` → `agfs_de_link(bp, d_type, in_base)`
- otherwise → `agfs_de_inode(ent.ino, gen, d_type, in_base)`

## Todos

| ID | Task | Depends on |
|----|------|------------|
| doc-update | Update `docs/staging.md`, `docs/internals.md`, and `docs/architecture.md`: replace dirent struct (5 fields) with `u64 packed` encoding layout, document `d_type` 2-bit private encoding and conversion helpers, replace terminology (staged→inode, redirect→link, deleted→tombstone), describe cancelled-entry removal, and update all pseudocode/helper references (`agfs_ino_is_staged` → `agfs_de_is_inode`, etc.) | — |
| struct-helpers | Replace five fields with `u64 packed` in `struct agfs_dirent`. Add encoders (`agfs_de_inode`, `agfs_de_link`) with `WARN_ON_ONCE` for out-of-range ino/gen/pointer, predicates (`agfs_de_is_inode`, `agfs_de_is_link`, `agfs_de_is_tombstone`), decoders (`agfs_de_ino`, `agfs_de_gen`, `agfs_de_base`, `agfs_de_d_type`, `agfs_de_in_base`, `agfs_de_emit_ino`), cleanup (`agfs_de_free_base`), `d_type` pack/unpack with WARN_ON for invalid values, and `BUILD_BUG_ON(ARCH_KMALLOC_MINALIGN < 8)` in `agfs.h`. Tombstone is just `packed == 0` (no constant needed). Remove old `agfs_ino_is_*` helpers. Keep `AGFS_INO_DELETED`/`AGFS_INO_REDIRECT` for UAPI only. | doc-update |
| staging-update | Update `staging.c`: `add_dirent` (cancelled-entry removal + packed ops), `del_dirent` (use tombstone constant), `free_de_buckets` (free link pointer), `do_cow` (packed init) | struct-helpers |
| lookup-update | Update `lookup.c`: inode/link/tombstone dispatch using packed helpers | struct-helpers |
| file-update | Update `file.c`: `read_dirent` returns packed, `emit_dirents` uses `agfs_de_emit_ino` + `agfs_de_d_type`. Gen comparison in `agfs_open_staged` masks `sbi->gen` to 15 bits. | struct-helpers |
| inode-update | Update `inode.c`: create and rename use packed encode/decode with new names | struct-helpers |
| ioctl-update | Update `ioctl.c`: restore inject maps UAPI ino to packed encoding | struct-helpers |
| test-encode | Add encode/decode round-trip tests: max ino (45-bit), max gen (15-bit), all `d_type` values, `in_base` true/false, link pointer recovery, tombstone zero-value, and `WARN_ON` triggers for out-of-range values | struct-helpers |
| test-dtype | Add `d_type` pack/unpack tests: valid values round-trip, invalid input returns `11` / `DT_UNKNOWN` | struct-helpers |
| test-cancel | Add cancelled-entry removal tests: delete of `in_base=false` entry removes it entirely; lookup and readdir confirm absence | staging-update |
| build-test | Build and run all tests in VM | staging-update, lookup-update, file-update, inode-update, ioctl-update, test-encode, test-dtype, test-cancel |
