# 76 — Drop `hide`; `deny` on a directory blocks listing

**Supersedes plan 75.** Plan 75 relocated `hide` into a dentry-local visibility
flag. This plan instead **removes `hide` entirely** and folds the useful part
of its motivation (don't let the agent *enumerate* a protected directory) into
`deny`. Parts B (access cache on the dentry) and the `perm`→`policy` rename
from 75 are already implemented and are **kept**; this plan builds on them.

## Rationale

`hide` provided *existence hiding* (ENOENT everywhere, name absent from the
parent listing). Its cost: a visibility concept threaded through six surfaces,
the pinned-hashed-dentry subtlety, and a `d_find_alias` in `yolo_permission`
(which produced a real hardlink bug). The one hard dependency people feared —
the mount-recursion guard (`hide_mountpoint`) — was shown **empirically not to
be load-bearing**: `yolo_lookup` resolves the lower path with
`lookup_one_unlocked` (non-mount-crossing), so the reflected mountpoint
resolves to the *underlying empty* directory and does not nest. Verified by
mounting yolofs without the guard (both `staging=0` and `staging=1`): the
reflected mountpoint exists but is empty, depth-2 does not exist, and a
depth-limited `find` terminates immediately. `FILESYSTEM_MAX_STACK_DEPTH`
(checked in `yolo_resolve_paths`) remains a hard backstop against unbounded
stacking regardless.

The paper's formal model (`paper/figures/model.tex`) already lists Policy as
`ask | allow | write-ask | read-only | deny` — no `hide`. This aligns code with
model.

## Accepted tradeoff (no "real hide")

Content protection is unchanged (agent still cannot read/write under `deny`).
What is given up is *existence hiding*:

- error codes distinguish existence: `deny` → EACCES, absent → ENOENT, so an
  agent that *guesses* a path can confirm it exists;
- the protected directory's own name stays visible in its parent's listing;
- `stat` on the protected path still succeeds;
- no per-entry hiding within a visible directory.

Bulk enumeration (`ls`, `find`, tree walks) is still stopped for a `deny`d
directory by the new deny-blocks-listing behavior below.

## New `deny` semantics on a directory

`deny` on a directory now **blocks listing its contents** (readdir), while
still permitting *traversal* to explicitly-allowed children (nearest-ancestor
wins is unchanged). So `deny /d` + `allow /d/ok` lets `open(/d/ok)` through but
`ls /d` fails.

- `yolo_dir_open`: when `perm.enabled` and the dir's resolved access is
  `YOLO_PERM_DENY` → `-EACCES`. (Only `DENY` blocks listing; `ask`/`write-ask`/
  `read-only`/`allow` list normally, as today.)
- File reads/writes/mutations under `deny` are already denied (unchanged).
- `stat`/traversal under `deny` unchanged (delegate to lower).

## Kernel changes

- **`enum yolo_perm`** (`yolofs.h`): remove `YOLO_PERM_HIDE`. Max policy is now
  `YOLO_PERM_DENY`.
- Remove `yolo_dentry_hidden` (`yolofs.h`) and `yolo_policy_to_access`
  (`perm.c`) — the HIDE→ASK mapping is moot; `yolo_access_store` stores the
  walked policy directly.
- **`yolo_permission`** (`inode.c`): collapse to — regular files return `0`
  (access gated in `yolo_open`; COW makes lower mode irrelevant); dirs/symlinks
  delegate to `inode_permission(lower)`. No `d_find_alias`, no hide.
- **`yolo_getattr`** (`inode.c`): drop the hide check → pure delegate.
- **`yolo_open`** (`file.c`): drop the `yolo_dentry_hidden` guard.
- **`yolo_lookup`** (`lookup.c`): drop the hide ENOENT/`d_drop` branch. Keep the
  `perm.enabled`-guarded `yolo_access_refresh`.
- **`yolo_dir_open`** (`dir.c`): replace the hide check with the deny-listing
  check above.
- **`yolo_base_entry_skipped`** (`dir.c`): drop the hide clause → skip only
  pinned overlay entries (tombstones/staged).
- **`policy_code`** (`journal.c`): drop the `YOLO_PERM_HIDE` case (enum gone).
- **`yolo_rule_set_ioctl`** (`ioctl.c`): bound check `rule.perm > YOLO_PERM_HIDE`
  → `> YOLO_PERM_DENY`; drop the hide-journal-skip condition (back to
  `if (rule.journal)`).

## Userspace changes

- `user/perm.rs`: remove `Perm::Hide` (enum, `to_ioctl`/`from_ioctl`, `FromStr`,
  `Display`).
- `user/ioctl.rs`: remove `YOLO_PERM_HIDE`.
- `user/main.rs`: remove `RuleAction::Hide` + handler; update `rule` help text.
- `user/cmd/watch.rs`: remove the `Perm::Hide => "is hidden"` arm.
- `user/config.rs`: remove `hide_mountpoint` and its call in `apply_rules`
  (no recursion guard needed).
- `user/journal/types.rs`: already has no `Policy::Hide` (removed under 75) —
  no change.

## Docs

- `docs/permissions.md`: remove the `hide` state and the deny-vs-hide section;
  document `deny` = no read/write **and no listing** for directories, with
  traversal-to-allowed-children preserved; drop the Landlock "information
  hiding" differentiator; update the "what is gated" table (readdir gated by
  `deny`; no lookup/getattr hide surface; no recursion-guard row).
- `docs/architecture.md`, `docs/staging.md`: drop hide references; the
  `yolo_base_entry_skipped` pseudocode skips only pinned entries.

## Tests

- Delete `tests/perm/test_hide.rs` and its `mod` entry.
- `tests/perm/test_live_rules.rs`: remove `hide_rule_unset_live_restores_visibility`.
- `tests/internals/test_journal_notes.rs`: drop the `hide`/unhide steps from
  `configure_record_format_and_noop_suppression` (hide is no longer a rule
  verb).
- Any CLI test invoking `yolo rule hide` → update/remove.
- **Add** deny-blocks-listing coverage (`tests/perm/`): `ls` of a `deny`d dir
  → EACCES; `open`/`stat` of an explicitly-`allow`ed child under it still work;
  a file read under `deny` still denied.
- Keep `ro_access_syscall_does_not_reflect_policy` (test_modes.rs).

## Steps

1. Docs (`permissions.md` first).
2. Kernel: drop hide + deny-blocks-listing.
3. Userspace: drop hide surfaces + `hide_mountpoint`.
4. Tests: remove hide tests, add deny-listing tests, fix journal/live-rules.
5. `make test`.
6. Code review (parallel sub-agents).
