# 77 — Drop the access cache; walk live per operation

## Rationale

The per-dentry access cache (`cached_access` + `cached_gen`) and the global
generation counter (`sbi->perm.gen`) exist to make permission resolution O(1)
on the hot path. That hot path was `->permission`, which fired per
path-component on every pathwalk. But after plans 75/76, `yolo_permission` no
longer resolves policy at all (regular files pass; dirs/symlinks delegate to
lower). The only remaining readers of the resolved access value are
**per-operation**:

- `yolo_perm_check_dentry` — once per `open` / mutate / `setattr`
- `yolo_readdir` — once per `getdents` (the `deny`-blocks-listing check)

At that frequency the cache saves almost nothing: `yolo_perm_walk` is O(depth)
over `d_parent` (typically 3–8 hops, up to the nearest rule-bearing ancestor),
on dentries the pathwalk just made hot — no locks, no allocation — against an
operation (`open` → lower `dentry_open` + possible COW) that costs 100–1000×
more. So the cache is paying its full complexity + memory cost to shave a
sub-1% slice off a few coarse operations.

Drop it and walk live. This also removes a correctness quirk: today a pure
rename does not bump the generation, so a renamed file keeps its pre-rename
resolved perm until the next bump; a live walk always resolves at the current
path.

## Changes (kernel)

Delete:
- `yolo_dentry_info.cached_access`, `cached_gen` (`yolofs.h`).
- `struct yolo_permission.gen` (`yolofs.h`), its init `atomic64_set(&sbi->perm.gen, 1)` (`super.c`), and both `atomic64_inc(&sbi->perm.gen)` bumps (`ioctl.c` RULE_SET + RESTORE/TRAVEL).
- `yolo_access_store`, `yolo_access_refresh`, `yolo_access_get` (`perm.c`).
- the `perm.enabled`-guarded `yolo_access_refresh` block in `yolo_lookup` (`lookup.c`) — lookup no longer warms a cache.
- the `yolo_access_store(check, perm)` call in `yolo_perm_ask` (`perm.c`) — it already walks; nothing to store.
- the `cached_*` init comment in `yolo_d_init` (`dentry.c`).

Change:
- `yolo_perm_check_dentry` (`perm.c`): `perm = yolo_perm_walk(check, NULL)` instead of `yolo_access_get(check)`.
- `yolo_readdir` (`dir.c`): `yolo_perm_walk(file->f_path.dentry, NULL) == YOLO_PERM_DENY`.

`yolo_perm_walk` is unchanged (still used by the ask path with `&source`, and by `RULE_RESOLVE`). Rule changes are visible immediately because every check reads live `dentry->policy` up the chain — no invalidation step needed.

Concurrency is unchanged: the walk already read `di->policy` lock-free up the parent chain (the gen counter never protected the walk, only the cache), and a rule is set under `di->lock` as a single store, so a concurrent walk sees either the old or new value — the same benign, tolerated race as before.

## Docs

- `permissions.md`: drop `cached_access`/`cached_gen` from the per-dentry table and `perm.gen` from the per-superblock table; simplify the access-resolution code block to `yolo_perm_walk` + live-walk callers; remove the "Cache Invalidation" section (rule changes are immediate; note the rename behavior is no longer a staleness quirk); update the Landlock comparison line ("O(1) via per-dentry cache + gen" → "O(depth) walk-up, no cache").
- `architecture.md`: the walkthrough should show `yolo_open` walking up at check time rather than `yolo_access_refresh` caching on the dentry.

## Tests

No new behavior; existing coverage suffices and gets stronger in meaning:
`live_rule_change_takes_effect`, `live_rule_remove_reapplies_gating`,
`live_deny_toggles_listing`, and `rule_resolve_matches_enforcement` all assert
a rule change takes effect on the next access — which now works via the live
walk with no generation counter. `make test` must stay green.

## Steps

1. Docs.
2. Kernel: delete cache + gen, walk live at the two call sites.
3. `make test`.
4. Code review (parallel sub-agents).
