# 30 — Dentry-centric dstate interface

## Problem

The dentry/dstate interface has two layers of indirection: callers reach
into `YOLO_D(dentry)->dstate` to get the raw `struct yolo_dstate`, then
call free-standing `yolo_dstate_*` helpers on it.  This creates several
problems:

1. **Two-step access pattern** — every query is
   `yolo_dstate_is_X(YOLO_D(dentry)->dstate)`.  The primary abstraction
   is the dentry, but the API operates on a secondary value extracted from
   it.

2. **Manual stage-or-overwrite** — three call sites (create, COW, rename)
   repeat the same pattern: check if unset, if so `yolo_stage_dentry`
   (which dgets), else `yolo_dstate_free` + direct assign.

3. **Leaked memory management** — callers must remember to call
   `yolo_dstate_free()` before overwriting a dstate that might be
   `base_path` (which embeds a kstrdup pointer).  Forgetting causes a leak.

4. **Naming inconsistency** — `yolo_stage_dentry(dentry, dstate)` takes a
   dentry, but `yolo_dstate_is_passthrough(dstate)` takes a raw dstate.
   The module lacks a single primary abstraction.

5. **Encoding exposed everywhere** — the packed u64 layout, dtype
   packing, pointer recovery are visible in `yolofs.h`.  Only `dentry.c`
   (and the restore deserializer) need these details.

## Approach

Replace the `yolo_dstate_*` public API with `yolo_dentry_*` functions
that take `struct dentry *` and encapsulate all dstate logic inside
`dentry.c`.

### 1. Dentry-centric queries

Replace two-step access patterns with single-call queries.  These are
`static inline` in `yolofs.h` for zero overhead:

```c
bool yolo_dentry_is_unset(const struct dentry *d);  /* val == 0 */
bool yolo_dentry_is_tombstone(const struct dentry *d);
bool yolo_dentry_is_staged_inode(const struct dentry *d);
bool yolo_dentry_is_base_path(const struct dentry *d);
bool yolo_dentry_in_base(const struct dentry *d);        /* unset: d_inode != NULL; staged: in_base flag */
bool yolo_dentry_is_current(const struct dentry *d, u16 gen);

unsigned char yolo_dentry_d_type(const struct dentry *d);
u32           yolo_dentry_ino(const struct dentry *d);
u16           yolo_dentry_gen(const struct dentry *d);
u64           yolo_dentry_emit_ino(const struct dentry *d);
const char   *yolo_dentry_base_src(const struct dentry *d);
```

### 2. Dentry-centric mutations

Replace `yolo_stage_dentry` / `yolo_dentry_unstage` / manual
stage-or-overwrite with three functions in `dentry.c`:

```c
void yolo_dentry_set_staged_ino(struct dentry *d, u32 ino, u16 gen,
                             unsigned char d_type, bool in_base);
void yolo_dentry_set_base_path(struct dentry *d, char *base_copy,
                                 unsigned char d_type, bool in_base);
void yolo_dentry_unstage(struct dentry *d);
```

Each mutation handles:
- `yolo_dstate_free()` on the old value if overwriting
- `dget()` if transitioning from unset to staged
- `dput()` if transitioning from staged to unset (reset)
- No-op safety (reset on unset is a no-op)

### 3. Rename tombstone operations

Rename existing functions to follow the `yolo_dentry_*` convention:

- `yolo_add_tombstone` → `yolo_dentry_add_tombstone`
- `yolo_remove_tombstone` → `yolo_dentry_remove_tombstone`

### 4. Move dstate encoding internals to `dentry.c`

Move from `yolofs.h` to `dentry.c` as `static` functions:
- `yolo_dtype_pack` / `yolo_dtype_unpack`
- All `yolo_dstate_is_*` predicates
- All `yolo_dstate_*` decoders (`d_type`, `in_base`, `ino`, `gen`, `src`,
  `emit_ino`)
- All `yolo_dstate_*` encoders (`staged_inode`, `base_path`, `tombstone`)
- `yolo_dstate_free`

`struct yolo_dstate { u64 val; }` remains in `yolofs.h` (it's embedded in
`yolo_dentry_info`), but callers no longer directly read or manipulate
`.val`.

### 5. Move `yolo_inject_dentry` to `dentry.c`

`yolo_inject_dentry` creates a dentry, sets its dstate and lower_path,
and adds it to the dcache.  This is fundamentally a dentry operation and
belongs in `dentry.c`.

Split into two type-specific functions (eliminates dstate parameter):

```c
int yolo_dentry_add_staged_inode(struct dentry *parent, const u8 *name,
                          u16 name_len, struct super_block *sb,
                          struct yolo_sb_info *sbi,
                          u32 ino, u16 gen,
                          unsigned char d_type, bool in_base);

int yolo_dentry_add_base_path(struct dentry *parent, const u8 *name,
                              u16 name_len, struct super_block *sb,
                              struct yolo_sb_info *sbi,
                              char *base_copy,
                              unsigned char d_type, bool in_base);
```

Tombstone injection uses `yolo_dentry_add_tombstone()` (renamed from
`yolo_add_tombstone()`).

### 6. Wire-format decoder for restore

The restore path in `ioctl.c` reads raw u64 dstate values from the wire
format.  Provide a decode helper so `ioctl.c` doesn't need to know the
bit layout:

```c
enum yolo_dstate_kind {
    YOLO_DSTATE_UNSET,
    YOLO_DSTATE_TOMBSTONE,
    YOLO_DSTATE_STAGED_INODE,
    YOLO_DSTATE_BASE_PATH,
};

struct yolo_dstate_wire {
    enum yolo_dstate_kind kind;
    unsigned char         d_type;
    bool                  in_base;
    u32                   ino;   /* valid for STAGED_INODE */
};

void yolo_dstate_decode_wire(u64 raw, struct yolo_dstate_wire *out);
```

The restore path becomes:

```c
yolo_dstate_decode_wire(raw_val, &w);
switch (w.kind) {
case YOLO_DSTATE_TOMBSTONE:
    yolo_dentry_add_tombstone(parent, name, len, w.d_type);
    break;
case YOLO_DSTATE_STAGED_INODE:
    yolo_dentry_add_staged_inode(parent, name, len, sb, sbi,
                          w.ino, gen, w.d_type, w.in_base);
    break;
case YOLO_DSTATE_BASE_PATH:
    /* read trailing base_src from buffer, kstrndup */
    yolo_dentry_add_base_path(parent, name, len, sb, sbi,
                              base_copy, w.d_type, w.in_base);
    break;
}
```

## Caller simplifications

### `yolo_create_staged` (inode.c)

Before:
```c
already_staged = !yolo_dstate_is_passthrough(di->dstate);
in_base = already_staged;
dstate = yolo_dstate_staged_inode(ino, gen, dt, in_base);
if (!already_staged)
    yolo_stage_dentry(dentry, dstate);
else
    di->dstate = dstate;
```

After:
```c
in_base = !yolo_dentry_is_unset(dentry);
yolo_dentry_set_staged_ino(dentry, ino, gen, dt, in_base);
```

### `yolo_do_cow` (staging.c)

Before:
```c
if (yolo_dstate_is_passthrough(di->dstate)) {
    yolo_stage_dentry(dentry,
        yolo_dstate_staged_inode(ino, gen, DT_REG, true));
} else {
    yolo_dstate_free(di->dstate);
    di->dstate = yolo_dstate_staged_inode(ino, gen, DT_REG, true);
}
```

After:
```c
yolo_dentry_set_staged_ino(dentry, ino, gen, DT_REG, true);
```

### `yolo_delete_entry` (inode.c)

Before:
```c
need_tombstone = yolo_dstate_is_passthrough(di->dstate) ||
                 yolo_dstate_in_base(di->dstate);
...
if (!yolo_dstate_is_passthrough(di->dstate))
    yolo_dentry_unstage(dentry);
```

After:
```c
need_tombstone = yolo_dentry_in_base(dentry);
...
yolo_dentry_unstage(dentry);   /* no-op if unset */
```

### `yolo_rename` (inode.c)

Before (state update, 14 lines):
```c
if (src_staged)
    yolo_dstate_free(src_dstate);
if (is_roundtrip) {
    old_di->dstate = (struct yolo_dstate){0};
    if (src_staged) dput(old_dentry);
} else if (src_staged) {
    old_di->dstate = dst_dstate;
} else {
    yolo_stage_dentry(old_dentry, dst_dstate);
}
```

After:
```c
if (is_roundtrip) {
    yolo_dentry_unstage(old_dentry);
} else if (yolo_dentry_is_staged_inode(old_dentry)) {
    yolo_dentry_set_staged_ino(old_dentry, yolo_dentry_ino(old_dentry),
                            yolo_dentry_gen(old_dentry),
                            d_type, dst_in_base);
} else {
    yolo_dentry_set_base_path(old_dentry, base_copy, d_type, dst_in_base);
    base_copy = NULL;  /* ownership transferred */
}
```

Also simplifies earlier reads:
```c
/* Before: */  src_staged = !yolo_dstate_is_passthrough(old_di->dstate);
/* After:  */  /* src_staged variable eliminated; use !yolo_dentry_is_unset() inline */

/* Before: */  if (yolo_dstate_is_passthrough(src_dstate) || yolo_dstate_in_base(src_dstate))
/* After:  */  if (yolo_dentry_in_base(old_dentry))

/* Before: */  if (src_staged && yolo_dstate_is_staged_inode(src_dstate)) base_src = NULL;
/* After:  */  if (yolo_dentry_is_staged_inode(old_dentry)) base_src = NULL;
```

### `yolo_emit_dirents` (dir.c)

Before:
```c
if (!di || yolo_dstate_is_passthrough(di->dstate)) continue;
if (yolo_dstate_is_tombstone(di->dstate)) continue;
dir_emit(..., yolo_dstate_emit_ino(di->dstate), yolo_dstate_d_type(di->dstate));
```

After:
```c
if (yolo_dentry_is_unset(child)) continue;
if (yolo_dentry_is_tombstone(child)) continue;
dir_emit(..., yolo_dentry_emit_ino(child), yolo_dentry_d_type(child));
```

### `yolo_fill_base` (dir.c)

Before:
```c
bool overridden = !yolo_dstate_is_passthrough(YOLO_D(child)->dstate);
```

After:
```c
bool overridden = !yolo_dentry_is_unset(child);
```

### `yolo_open_staged` (file.c)

Before:
```c
dstate = YOLO_D(dentry)->dstate;
if (yolo_dstate_is_current(dstate, gen)) {
    ...
    return yolo_open_staged_ino(sbi, yolo_dstate_ino(dstate), ...);
}
```

After:
```c
if (yolo_dentry_is_current(dentry, gen)) {
    ...
    return yolo_open_staged_ino(sbi, yolo_dentry_ino(dentry), ...);
}
```

## Functions removed

| Old function | Replacement |
|---|---|
| `yolo_stage_dentry(dentry, dstate)` | `yolo_dentry_set_staged_ino` / `yolo_dentry_set_base_path` |
| `yolo_unstage_dentry(dentry)` | `yolo_dentry_unstage` (with no-op safety) |
| `yolo_dstate_is_passthrough(p)` | `yolo_dentry_is_unset(d)` |
| `yolo_dstate_is_tombstone(p)` | `yolo_dentry_is_tombstone(d)` |
| `yolo_dstate_is_base_path(p)` | `yolo_dentry_is_base_path(d)` |
| `yolo_dstate_is_staged_inode(p)` | `yolo_dentry_is_staged_inode(d)` |
| `yolo_dstate_is_current(p, gen)` | `yolo_dentry_is_current(d, gen)` |
| `yolo_dstate_d_type(p)` | `yolo_dentry_d_type(d)` |
| `yolo_dstate_in_base(p)` | `yolo_dentry_in_base(d)` |
| `yolo_dstate_ino(p)` | `yolo_dentry_ino(d)` |
| `yolo_dstate_gen(p)` | `yolo_dentry_gen(d)` |
| `yolo_dstate_src(p)` | `yolo_dentry_base_src(d)` |
| `yolo_dstate_emit_ino(p)` | `yolo_dentry_emit_ino(d)` |
| `yolo_dstate_staged_inode(...)` | internal to `dentry.c` |
| `yolo_dstate_base_path(...)` | internal to `dentry.c` |
| `yolo_dstate_tombstone(...)` | internal to `dentry.c` |
| `yolo_dstate_free(p)` | internal to `dentry.c` |
| `yolo_dtype_pack/unpack` | internal to `dentry.c` |
| `yolo_inject_dentry(...)` | `yolo_dentry_add_staged_inode` / `yolo_dentry_add_base_path` (in `dentry.c`) |
| `yolo_add_tombstone(...)` | `yolo_dentry_add_tombstone` |
| `yolo_remove_tombstone(...)` | `yolo_dentry_remove_tombstone` |

## Files affected

| File | Change |
|---|---|
| `yolofs.h` | Remove `yolo_dstate_*` and `yolo_dtype_*` inlines; add `yolo_dentry_*` query inlines and extern declarations for mutations/inject |
| `dentry.c` | Add `yolo_dentry_set_staged_ino`, `yolo_dentry_set_base_path`, `yolo_dentry_unstage`; rename `yolo_add_tombstone` → `yolo_dentry_add_tombstone`, `yolo_remove_tombstone` → `yolo_dentry_remove_tombstone`; move all dstate encoding internals here; move `yolo_inject_dentry` from `ioctl.c` (split into `yolo_dentry_add_staged_inode` / `yolo_dentry_add_base_path`); add `yolo_dstate_decode_wire` |
| `inode.c` | Rewrite `yolo_create_staged`, `yolo_delete_entry`, `yolo_rename` to use new API |
| `staging.c` | Rewrite `yolo_do_cow` to use `yolo_dentry_set_staged_ino` |
| `file.c` | Rewrite `yolo_open_staged` to use `yolo_dentry_is_current` / `yolo_dentry_ino` |
| `dir.c` | Rewrite `yolo_emit_dirents` / `yolo_fill_base` to use `yolo_dentry_*` queries |
| `ioctl.c` | Rewrite `yolo_restore_inject` to use `yolo_dstate_decode_wire` + `yolo_dentry_add_staged_inode` / `yolo_dentry_add_base_path` / `yolo_dentry_add_tombstone` |
