# 30 — Dentry-centric dstate interface

## Problem

The dentry/dstate interface has two layers of indirection: callers reach
into `AGFS_D(dentry)->dstate` to get the raw `struct agfs_dstate`, then
call free-standing `agfs_dstate_*` helpers on it.  This creates several
problems:

1. **Two-step access pattern** — every query is
   `agfs_dstate_is_X(AGFS_D(dentry)->dstate)`.  The primary abstraction
   is the dentry, but the API operates on a secondary value extracted from
   it.

2. **Manual stage-or-overwrite** — three call sites (create, COW, rename)
   repeat the same pattern: check if passthrough, if so `agfs_stage_dentry`
   (which dgets), else `agfs_dstate_free` + direct assign.

3. **Leaked memory management** — callers must remember to call
   `agfs_dstate_free()` before overwriting a dstate that might be
   `base_path` (which embeds a kstrdup pointer).  Forgetting causes a leak.

4. **Naming inconsistency** — `agfs_stage_dentry(dentry, dstate)` takes a
   dentry, but `agfs_dstate_is_passthrough(dstate)` takes a raw dstate.
   The module lacks a single primary abstraction.

5. **Encoding exposed everywhere** — the packed u64 layout, dtype
   packing, pointer recovery are visible in `agfs.h`.  Only `dentry.c`
   (and the restore deserializer) need these details.

## Approach

Replace the `agfs_dstate_*` public API with `agfs_dentry_*` functions
that take `struct dentry *` and encapsulate all dstate logic inside
`dentry.c`.

### 1. Dentry-centric queries

Replace two-step access patterns with single-call queries.  These are
`static inline` in `agfs.h` for zero overhead:

```c
bool agfs_dentry_is_passthrough(const struct dentry *d);  /* val == 0 */
bool agfs_dentry_is_tombstone(const struct dentry *d);
bool agfs_dentry_is_staged_inode(const struct dentry *d);
bool agfs_dentry_is_base_path(const struct dentry *d);
bool agfs_dentry_in_base(const struct dentry *d);        /* passthrough: d_inode != NULL; staged: in_base flag */
bool agfs_dentry_is_current(const struct dentry *d, u16 gen);

unsigned char agfs_dentry_d_type(const struct dentry *d);
u32           agfs_dentry_ino(const struct dentry *d);
u16           agfs_dentry_gen(const struct dentry *d);
u64           agfs_dentry_emit_ino(const struct dentry *d);
const char   *agfs_dentry_base_src(const struct dentry *d);
```

### 2. Dentry-centric mutations

Replace `agfs_stage_dentry` / `agfs_dentry_reset` / manual
stage-or-overwrite with three functions in `dentry.c`:

```c
void agfs_dentry_set_staged_ino(struct dentry *d, u32 ino, u16 gen,
                             unsigned char d_type, bool in_base);
void agfs_dentry_set_base_path(struct dentry *d, char *base_copy,
                                 unsigned char d_type, bool in_base);
void agfs_dentry_reset(struct dentry *d);
```

Each mutation handles:
- `agfs_dstate_free()` on the old value if overwriting
- `dget()` if transitioning from passthrough to staged
- `dput()` if transitioning from staged to passthrough (reset)
- No-op safety (reset on passthrough is a no-op)

### 3. Rename tombstone operations

Rename existing functions to follow the `agfs_dentry_*` convention:

- `agfs_add_tombstone` → `agfs_dentry_add_tombstone`
- `agfs_remove_tombstone` → `agfs_dentry_remove_tombstone`

### 4. Move dstate encoding internals to `dentry.c`

Move from `agfs.h` to `dentry.c` as `static` functions:
- `agfs_dtype_pack` / `agfs_dtype_unpack`
- All `agfs_dstate_is_*` predicates
- All `agfs_dstate_*` decoders (`d_type`, `in_base`, `ino`, `gen`, `src`,
  `emit_ino`)
- All `agfs_dstate_*` encoders (`staged_inode`, `base_path`, `tombstone`)
- `agfs_dstate_free`

`struct agfs_dstate { u64 val; }` remains in `agfs.h` (it's embedded in
`agfs_dentry_info`), but callers no longer directly read or manipulate
`.val`.

### 5. Move `agfs_inject_dentry` to `dentry.c`

`agfs_inject_dentry` creates a dentry, sets its dstate and lower_path,
and adds it to the dcache.  This is fundamentally a dentry operation and
belongs in `dentry.c`.

Split into two type-specific functions (eliminates dstate parameter):

```c
int agfs_dentry_add_staged_inode(struct dentry *parent, const u8 *name,
                          u16 name_len, struct super_block *sb,
                          struct agfs_sb_info *sbi,
                          u32 ino, u16 gen,
                          unsigned char d_type, bool in_base);

int agfs_dentry_add_base_path(struct dentry *parent, const u8 *name,
                              u16 name_len, struct super_block *sb,
                              struct agfs_sb_info *sbi,
                              char *base_copy,
                              unsigned char d_type, bool in_base);
```

Tombstone injection uses `agfs_dentry_add_tombstone()` (renamed from
`agfs_add_tombstone()`).

### 6. Wire-format decoder for restore

The restore path in `ioctl.c` reads raw u64 dstate values from the wire
format.  Provide a decode helper so `ioctl.c` doesn't need to know the
bit layout:

```c
enum agfs_dstate_kind {
    AGFS_DSTATE_PASSTHROUGH,
    AGFS_DSTATE_TOMBSTONE,
    AGFS_DSTATE_STAGED_INODE,
    AGFS_DSTATE_BASE_PATH,
};

struct agfs_dstate_wire {
    enum agfs_dstate_kind kind;
    unsigned char         d_type;
    bool                  in_base;
    u32                   ino;   /* valid for STAGED_INODE */
};

void agfs_dstate_decode_wire(u64 raw, struct agfs_dstate_wire *out);
```

The restore path becomes:

```c
agfs_dstate_decode_wire(raw_val, &w);
switch (w.kind) {
case AGFS_DSTATE_TOMBSTONE:
    agfs_dentry_add_tombstone(parent, name, len, w.d_type);
    break;
case AGFS_DSTATE_STAGED_INODE:
    agfs_dentry_add_staged_inode(parent, name, len, sb, sbi,
                          w.ino, gen, w.d_type, w.in_base);
    break;
case AGFS_DSTATE_BASE_PATH:
    /* read trailing base_src from buffer, kstrndup */
    agfs_dentry_add_base_path(parent, name, len, sb, sbi,
                              base_copy, w.d_type, w.in_base);
    break;
}
```

## Caller simplifications

### `agfs_create_staged` (inode.c)

Before:
```c
already_staged = !agfs_dstate_is_passthrough(di->dstate);
in_base = already_staged;
dstate = agfs_dstate_staged_inode(ino, gen, dt, in_base);
if (!already_staged)
    agfs_stage_dentry(dentry, dstate);
else
    di->dstate = dstate;
```

After:
```c
in_base = !agfs_dentry_is_passthrough(dentry);
agfs_dentry_set_staged_ino(dentry, ino, gen, dt, in_base);
```

### `agfs_do_cow` (staging.c)

Before:
```c
if (agfs_dstate_is_passthrough(di->dstate)) {
    agfs_stage_dentry(dentry,
        agfs_dstate_staged_inode(ino, gen, DT_REG, true));
} else {
    agfs_dstate_free(di->dstate);
    di->dstate = agfs_dstate_staged_inode(ino, gen, DT_REG, true);
}
```

After:
```c
agfs_dentry_set_staged_ino(dentry, ino, gen, DT_REG, true);
```

### `agfs_delete_entry` (inode.c)

Before:
```c
need_tombstone = agfs_dstate_is_passthrough(di->dstate) ||
                 agfs_dstate_in_base(di->dstate);
...
if (!agfs_dstate_is_passthrough(di->dstate))
    agfs_dentry_reset(dentry);
```

After:
```c
need_tombstone = agfs_dentry_in_base(dentry);
...
agfs_dentry_reset(dentry);   /* no-op if passthrough */
```

### `agfs_rename` (inode.c)

Before (state update, 14 lines):
```c
if (src_staged)
    agfs_dstate_free(src_dstate);
if (is_roundtrip) {
    old_di->dstate = (struct agfs_dstate){0};
    if (src_staged) dput(old_dentry);
} else if (src_staged) {
    old_di->dstate = dst_dstate;
} else {
    agfs_stage_dentry(old_dentry, dst_dstate);
}
```

After:
```c
if (is_roundtrip) {
    agfs_dentry_reset(old_dentry);
} else if (agfs_dentry_is_staged_inode(old_dentry)) {
    agfs_dentry_set_staged_ino(old_dentry, agfs_dentry_ino(old_dentry),
                            agfs_dentry_gen(old_dentry),
                            d_type, dst_in_base);
} else {
    agfs_dentry_set_base_path(old_dentry, base_copy, d_type, dst_in_base);
    base_copy = NULL;  /* ownership transferred */
}
```

Also simplifies earlier reads:
```c
/* Before: */  src_staged = !agfs_dstate_is_passthrough(old_di->dstate);
/* After:  */  /* src_staged variable eliminated; use !agfs_dentry_is_passthrough() inline */

/* Before: */  if (agfs_dstate_is_passthrough(src_dstate) || agfs_dstate_in_base(src_dstate))
/* After:  */  if (agfs_dentry_in_base(old_dentry))

/* Before: */  if (src_staged && agfs_dstate_is_staged_inode(src_dstate)) base_src = NULL;
/* After:  */  if (agfs_dentry_is_staged_inode(old_dentry)) base_src = NULL;
```

### `agfs_emit_dirents` (dir.c)

Before:
```c
if (!di || agfs_dstate_is_passthrough(di->dstate)) continue;
if (agfs_dstate_is_tombstone(di->dstate)) continue;
dir_emit(..., agfs_dstate_emit_ino(di->dstate), agfs_dstate_d_type(di->dstate));
```

After:
```c
if (!di || !!agfs_dentry_is_passthrough(child)) continue;
if (agfs_dentry_is_tombstone(child)) continue;
dir_emit(..., agfs_dentry_emit_ino(child), agfs_dentry_d_type(child));
```

### `agfs_fill_base` (dir.c)

Before:
```c
bool overridden = !agfs_dstate_is_passthrough(AGFS_D(child)->dstate);
```

After:
```c
bool overridden = !agfs_dentry_is_passthrough(child);
```

### `agfs_open_staged` (file.c)

Before:
```c
dstate = AGFS_D(dentry)->dstate;
if (agfs_dstate_is_current(dstate, gen)) {
    ...
    return agfs_open_staged_ino(sbi, agfs_dstate_ino(dstate), ...);
}
```

After:
```c
if (agfs_dentry_is_current(dentry, gen)) {
    ...
    return agfs_open_staged_ino(sbi, agfs_dentry_ino(dentry), ...);
}
```

## Functions removed

| Old function | Replacement |
|---|---|
| `agfs_stage_dentry(dentry, dstate)` | `agfs_dentry_set_staged_ino` / `agfs_dentry_set_base_path` |
| `agfs_dentry_reset(dentry)` | `agfs_dentry_reset` (with no-op safety) |
| `agfs_dstate_is_passthrough(p)` | `!agfs_dentry_is_passthrough(d)` (inverted) |
| `agfs_dstate_is_tombstone(p)` | `agfs_dentry_is_tombstone(d)` |
| `agfs_dstate_is_base_path(p)` | `agfs_dentry_is_base_path(d)` |
| `agfs_dstate_is_staged_inode(p)` | `agfs_dentry_is_staged_inode(d)` |
| `agfs_dstate_is_current(p, gen)` | `agfs_dentry_is_current(d, gen)` |
| `agfs_dstate_d_type(p)` | `agfs_dentry_d_type(d)` |
| `agfs_dstate_in_base(p)` | `agfs_dentry_in_base(d)` |
| `agfs_dstate_ino(p)` | `agfs_dentry_ino(d)` |
| `agfs_dstate_gen(p)` | `agfs_dentry_gen(d)` |
| `agfs_dstate_src(p)` | `agfs_dentry_base_src(d)` |
| `agfs_dstate_emit_ino(p)` | `agfs_dentry_emit_ino(d)` |
| `agfs_dstate_staged_inode(...)` | internal to `dentry.c` |
| `agfs_dstate_base_path(...)` | internal to `dentry.c` |
| `agfs_dstate_tombstone(...)` | internal to `dentry.c` |
| `agfs_dstate_free(p)` | internal to `dentry.c` |
| `agfs_dtype_pack/unpack` | internal to `dentry.c` |
| `agfs_inject_dentry(...)` | `agfs_dentry_add_staged_inode` / `agfs_dentry_add_base_path` (in `dentry.c`) |
| `agfs_add_tombstone(...)` | `agfs_dentry_add_tombstone` |
| `agfs_remove_tombstone(...)` | `agfs_dentry_remove_tombstone` |

## Files affected

| File | Change |
|---|---|
| `agfs.h` | Remove `agfs_dstate_*` and `agfs_dtype_*` inlines; add `agfs_dentry_*` query inlines and extern declarations for mutations/inject |
| `dentry.c` | Add `agfs_dentry_set_staged_ino`, `agfs_dentry_set_base_path`, `agfs_dentry_reset`; rename `agfs_add_tombstone` → `agfs_dentry_add_tombstone`, `agfs_remove_tombstone` → `agfs_dentry_remove_tombstone`; move all dstate encoding internals here; move `agfs_inject_dentry` from `ioctl.c` (split into `agfs_dentry_add_staged_inode` / `agfs_dentry_add_base_path`); add `agfs_dstate_decode_wire` |
| `inode.c` | Rewrite `agfs_create_staged`, `agfs_delete_entry`, `agfs_rename` to use new API |
| `staging.c` | Rewrite `agfs_do_cow` to use `agfs_dentry_set_staged_ino` |
| `file.c` | Rewrite `agfs_open_staged` to use `agfs_dentry_is_current` / `agfs_dentry_ino` |
| `dir.c` | Rewrite `agfs_emit_dirents` / `agfs_fill_base` to use `agfs_dentry_*` queries |
| `ioctl.c` | Rewrite `agfs_restore_inject` to use `agfs_dstate_decode_wire` + `agfs_dentry_add_staged_inode` / `agfs_dentry_add_base_path` / `agfs_dentry_add_tombstone` |
