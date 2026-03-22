# 28 — Roundtrip rename detection

## Problem

When a base file is renamed back to its original path (e.g. `mv a tmp && mv tmp a`),
the kernel emits two journal Rename records and sets a `BasePath` dstate pointing back
to the original path. The CLI resolves this to `Passthrough` (no net change), but the
kernel still allocates a `kstrdup` for the base-path string that will be discarded.
For longer chains (a→b→c→a) the same overhead applies.

## Approach

Detect roundtrip renames at rename time in the kernel: when the effective base source
path (`base_src`) equals the destination relpath, the dentry is left as passthrough
(zeroed dstate) and `kstrdup` is skipped.

### Kernel changes (`kmod/inode.c` — `agfs_rename`)

1. After computing `base_src` (the effective source path through rename chains),
   call `dentry_path_raw` on `new_dentry` to get the destination relpath.
2. Compare `base_src` with the destination relpath via `strcmp`.
3. If equal (`is_roundtrip`), skip `kstrdup`/`agfs_dstate_base_path` and set
   `old_di->dstate = (struct agfs_dstate){0}` (passthrough).
4. The journal still records the R/P entries — the CLI has its own roundtrip detection.

### Ancillary cleanup (`kmod/staging.c`, `kmod/file.c`)

Removed `agfs_dentry_relpath` wrapper; call sites now use `dentry_path_raw` directly.

### Documentation (`docs/staging.md`)

Added description of roundtrip rename detection under the rename section.

## Files affected

- `kmod/inode.c` — roundtrip detection in `agfs_rename`
- `kmod/staging.c` — removed `agfs_dentry_relpath` helper
- `kmod/agfs.h` — removed `agfs_dentry_relpath` declaration
- `kmod/file.c` — inline `dentry_path_raw` call
- `docs/staging.md` — document the optimization
