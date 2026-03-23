# 24 — Passthrough dstate + tombstone with d_type

## Problem

The kernel `agfs_dstate` uses `val == 0` for tombstone, which collides with the
zero-initialized default.  There is no representation for "this dentry is
passthrough" (follows base state).  Additionally, tombstones lack a `d_type`
field, requiring callers to infer it from other sources.

## Design

Add a fourth state to `agfs_dstate`:

| State     | Condition                      | Meaning                         |
|-----------|--------------------------------|---------------------------------|
| Passthrough | `val == 0`                     | Default; follows base filesystem|
| Tombstone | `(s64)val > 0 && ino == 0`     | Deleted; has d_type, in_base=1  |
| Inode     | `(s64)val > 0 && ino != 0`     | Staged content (unchanged)      |
| Link      | `(s64)val < 0`                 | Base redirect (unchanged)       |

Tombstone layout:

    [63]    0        (tag)
    [62:60] d_type   3 bits
    [59]    1        (in_base, always true)
    [58:0]  0        (reserved + ino=0 + gen=0)

Key properties:
- Zero-initialized dentries are now "passthrough" (correct default semantics).
- Tombstones carry d_type and always have in_base=true by construction.
- `agfs_dstate_in_base()` simplifies to `(val >> 59) & 1` for all states
  (passthrough gives 0, tombstone gives 1, inode/link read the bit).

## Changes

### kmod/agfs.h
1. Update encoding comment (four states).
2. Add `agfs_dstate_is_passthrough()` predicate.
3. Change `agfs_dstate_is_tombstone()`: `(s64)val > 0 && ino-bits == 0`.
4. Change `agfs_dstate_is_staged_inode()`: `(s64)val > 0 && ino-bits != 0`.
5. Simplify `agfs_dstate_in_base()`: just `(val >> 59) & 1`.
6. Add `agfs_dstate_tombstone(unsigned char d_type)` encoder.
7. Update `agfs_add_tombstone` declaration to take `d_type`.

### kmod/dentry.c
1. Update comments in `agfs_d_init` and `agfs_unstage_dentry` (packed=0 is
   passthrough).
2. `agfs_add_tombstone`: add `d_type` parameter, set
   `packed = agfs_dstate_tombstone(d_type)`.

### kmod/inode.c
1. `agfs_delete_entry`: pass `d_type` to `agfs_add_tombstone`.
2. `agfs_rename`: pass `d_type` to `agfs_add_tombstone`.

### kmod/ioctl.c
1. Restore parsing: `packed == 0` → passthrough (skip staging packed);
   `(s64)packed > 0 && ino == 0` → tombstone (set packed from wire value).

### docs/staging.md
1. Update encoding table and tombstone references.

## Relationship to plan 23

Plan 23 adds `Dirent::Passthrough` and `Tombstone { dtype }` on the CLI side.
This plan is the kernel-side counterpart.  The open question in plan 23 —
"Should the kernel packed format encode dtype in tombstones?" — is answered
yes.
