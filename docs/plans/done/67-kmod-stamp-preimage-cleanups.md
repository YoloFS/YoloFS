# 67 — kmod cleanups: stamp helper, preimage relocation, banner/naming

Pure internal refactors in `kmod/`. No behavior change, no ABI change, no
doc-visible behavior change — so no `docs/` updates are required (the
`docs/staging.md` pseudocode describing `staging_gen = sbi->gen` stays
accurate; it documents *what* happens, not the helper it flows through).

Follow-up to review items #2/#3/#4 from the kmod review.

## 1. Centralize the staged-inode stamp

The `staging_gen` + `staging_ino` pair is written verbatim in three places,
and the invariant it encodes is read only by `yolo_dentry_is_current()`. Put
the write next to the read.

Add to `yolofs.h`, right after `yolo_dentry_is_current()`:

```c
/* Stamp @d as a staged inode at generation @gen backed by store ino @ino.
 * Pairs with yolo_dentry_is_current(): a stamp at the live gen reads as
 * current (write-in-place); a lower gen forces re-COW on the next write. */
static inline void yolo_stamp_staged(const struct dentry *d, u16 gen, u32 ino)
{
	YOLO_I(d_inode(d))->staging_gen = gen;
	YOLO_I(d_inode(d))->staging_ino = ino;
}
```

Callers:
- `inode.c` `yolo_stage_inode()` → `yolo_stamp_staged(dentry, (u16)atomic_read(&sbi->staging.gen), ino)`
- `staging.c` `yolo_do_cow()` → same
- `ioctl.c` `travel_inject_entry()` → `yolo_stamp_staged(child, (ino > cow_ino_floor) ? gen : (gen ? gen - 1 : 0), ino)`

## 2. Move `yolo_preimage_target` out of the header

It is a ~30-line `static inline` in `yolofs.h` used by only `journal.c` (3
sites) and `staging.c` (1). Move the body + comment into `journal.c` (its
natural home — it produces journal pre-image tags) and leave an `extern`
declaration under the `/* journal.c */` section of `yolofs.h`.

Place the definition under `journal.c`'s existing empty `── Helpers ──`
banner (see #3), so it lands where a helper belongs.

## 3. Journal dead banner (#4a)

`journal.c` has an empty `── Helpers ──` banner immediately followed by
`── Public: typed journal record writers ──`. Filling it with
`yolo_preimage_target` (from #2) resolves this — `op_char`/`decision_char`
stay next to their only users (the ask/block writers).

## 4. Naming consistency (#4b)

Rename `get_shard_dir` → `yolo_get_shard_dir` (`staging.c` def + one call
site), and update the stale reference in the `ioctl.c` quiesce comment.

## Verification

- `make kmod` compiles clean (no new warnings).
- `make test-vm` (unit + e2e in VM) — behavior is unchanged, so the existing
  staging/travel/journal suites are the regression guard.
