# 60 — Namespace the perm API as yolo_perm_*

Follow-up to plan 59 (user request). The perm layer's exported functions mix
verb-object names (`yolo_check_perm`, `yolo_resolve_perm`) with noun-shaped
ones (`yolo_effective_perm`), and `yolo_cache_perm` is ambiguous about
direction (it *writes* the cache). Rename the perm.c API to a `yolo_perm_*`
namespace that makes the layering mechanically obvious (the file-local
`yolo_perm_needs_ask` already follows it):

| old | new | role |
|---|---|---|
| `yolo_resolve_perm` | `yolo_perm_walk` | slow path: walk the dentry chain for the governing rule, optionally report its source |
| `yolo_cache_perm` | `yolo_perm_refresh` | walk + store on the inode, stamp `perm_gen` |
| `yolo_effective_perm` | `yolo_perm_get` | fast path: cached read, refreshing if stale |
| `yolo_check_perm` | `yolo_perm_check` | perm × open-flags → 0 / -EACCES / -ENOENT |
| `yolo_check_dentry_perm` | `yolo_perm_check_dentry` | full resolve → ask → check pipeline |

Also: `yolo_check_perm` is called only inside perm.c — make it static and
remove it from yolofs.h.

Out of scope: the ask protocol (`yolo_ask_userspace`, `yolo_ask_release` —
their namespace is `yolo_ask_*`), journal helpers, and file-local helpers in
other files (`yolo_check_mutate_perm` in inode.c stays file-local style).

Docs: update the pseudocode and prose references in `docs/permissions.md`
and the flow diagrams in `docs/architecture.md` to the new names.

Pure rename, no behavior change. Verify with `make kmod`, `make test-vm`,
and the standard parallel review.
