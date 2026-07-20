# 75 — Consolidate permission state on the dentry (split visibility from access)

> **Superseded by plan 76.** Parts B (access cache on the dentry) and the
> `perm`→`policy` rename here are implemented and kept. Part A (relocating
> `hide` into a visibility flag) is reverted by plan 76, which removes `hide`
> entirely and folds enumeration control into `deny`.

## Motivation

`enum yolo_perm` and the permission machinery conflate several distinct
concerns. This plan separates them and puts all permission state where it
belongs — on the dentry — landing three interlocking changes as one refactor
(they touch the same files, so splitting them means editing the same code
three times):

- **(A) `hide` is not an access state.** It is a *visibility/namespace*
  property (does the name exist?), yet it rides the same resolve→cache→check
  pipeline as the *access* states (`allow`/`deny`/`ask`/`read-only`/
  `write-ask`). That forces a `HIDE` branch into the dentry-less
  `yolo_permission` (resolved via `d_find_alias`), a `case YOLO_PERM_HIDE` in
  `yolo_perm_check`, and inconsistent `cached_perm == HIDE` reads at four
  sites (readdir already reads the *own rule*; the rest read the *inherited*
  cache).
- **(B) The resolved access perm is cached on the wrong object.** It lives on
  the inode (`cached_perm`/`perm_gen`) while rules are name-based, so hardlinks
  with divergent rules share one cached value, and the dentry-less
  `yolo_permission` must guess a name via `d_find_alias`.
- **(C) `yolo_permission` re-implements the access decision.** The regular-file
  `switch` in `yolo_permission` duplicates `yolo_perm_check`, and is the only
  reason a dentry-less caller needs the access cache at all.

End state: all permission state is dentry-local, `open` is the single
authority for file access, there is no `d_find_alias` guessing, `hide` is a
visibility flag, and hidden paths produce no journal noise.

## The enum

A path's **policy** is exactly one mutually-exclusive choice from
`{unset, ask, allow, write-ask, read-only, deny, hide}` — that mutual
exclusivity is what an enum models well, so `hide` stays a member of the
*policy* enum (`YOLO_PERM_HIDE` in `yolo_dentry_info.perm`, the ioctl, and
`yolofs.toml` are unchanged). Splitting `hide` into a separate `bool` would
create a two-field "at most one active" invariant — worse modeling.

The real conflation is that `enum yolo_perm` also types the *resolved access
decision*. After this plan, the resolved/cached value is **never** `HIDE` (by
construction — the access refresh maps a resolved `HIDE` to `ASK`).

**Decided: no separate `enum yolo_access`.** We keep a single `enum yolo_perm`
as the umbrella and rely on the invariant "`cached_access` is never `HIDE`"
rather than a distinct type. Bounds churn (no `YOLO_PERM_*` sweep across the
ioctl and Rust).

## Part A — `hide` as a dentry-local visibility flag

### Key insight: `hide` needs no inheritance

A hidden subtree is unreachable *because you ENOENT at the boundary node* —
you cannot traverse into a hidden directory to reach its children. So at any
**reachable** node, "is this hidden?" is decided entirely by **that node's own
rule**; if an ancestor were hidden you could not be at this node. Walk-up
inheritance of `hide` is redundant for enforcement.

Worked cases for `hide ~/.mozilla`:

- `stat ~/.mozilla/cache/foo` → walk hits `.mozilla` (lookup/traversal) → own
  rule `HIDE` → ENOENT. Never reaches the descendant.
- `readdir ~` → child `.mozilla` own rule `HIDE` → skip.
- already-cached `~/.mozilla/cache` by full path → directory traversal
  `->permission(.mozilla, MAY_EXEC)` → own rule `HIDE` → ENOENT.

### Visibility check — own-rule only

```c
/* Visibility is a dentry-local property: hidden iff this node's OWN rule says
 * so. No walk-up — hidden subtrees are unreachable past their boundary. */
static inline bool yolo_dentry_hidden(struct dentry *dentry)
{
    return YOLO_SB(dentry->d_sb)->perm.enabled &&
           YOLO_D(dentry)->perm == YOLO_PERM_HIDE;
}
```

### Enforcement surfaces (all read the own rule, all hold a dentry)

| Surface | Site | Change |
|---|---|---|
| fresh lookup | [lookup.c:135](../../kmod/lookup.c#L135) | `yolo_dentry_hidden(dentry)` → `d_drop` + `-ENOENT` |
| parent enumeration | [dir.c:123](../../kmod/dir.c#L123) | already own-rule; route through helper |
| dir traversal (cached-descendant case) | [inode.c:227](../../kmod/inode.c#L227) `yolo_permission` | keep HIDE→ENOENT **for directories only**, via `yolo_dentry_hidden` on the dir |
| stat | [inode.c:344](../../kmod/inode.c#L344) `yolo_getattr` | `yolo_dentry_hidden(dentry)` (drop the `yolo_perm_get` HIDE read) |
| readdir open | [dir.c:35](../../kmod/dir.c#L35) `yolo_dir_open` | `yolo_dentry_hidden(dentry)` |
| open | [file.c:119](../../kmod/file.c#L119) `yolo_open` | explicit `yolo_dentry_hidden` → `-ENOENT` guard before the access check |

The dir-traversal case is the only surface testing a node other than the one
named; it tests the ancestor's **own** rule — still no inheritance.

### Journal — no record for `hide`

`hide` is a visibility concern, not an access policy; hidden paths are already
excluded from `G` logging. Remove them from the policy journal too:

- ioctl `RULE_SET`: skip `yolo_journal_configure` when the assignment involves
  hide either direction (`old_perm == HIDE || new == HIDE`) —
  [ioctl.c:255](../../kmod/ioctl.c#L255).
- drop `case YOLO_PERM_HIDE → 'h'` from `policy_code`
  ([journal.c:279](../../kmod/journal.c#L279)).
- drop `Policy::Hide` / `b'h'` from
  [user/journal/types.rs](../../user/journal/types.rs#L120).

## Part B — access cache on the dentry

Move the resolved-access cache off the inode onto the dentry:

- `yolo_dentry_info`: add `cached_access` (resolved access perm, never `HIDE`)
  + `cached_gen` (the `sb.perm.gen` value the cache was stamped at).
- `yolo_inode_info`: drop `cached_perm` + `perm_gen`.
- `yolo_access_get`/`yolo_access_refresh` (renamed from `yolo_perm_get`/
  `yolo_perm_refresh`) operate on the dentry; keep the global `sb.perm.gen`
  for O(1) lazy invalidation (bump gen → each dentry re-walks on next access).
  This fixes the hardlink hole (per-name cache) and keeps O(1) steady-state
  checks.
- `yolo_perm_walk` is unchanged (still used by `RULE_RESOLVE`); the refresh
  that feeds `cached_access` maps a resolved `HIDE` to the default `ASK`
  (unreachable for a reachable node, but keeps the access value total).

## Naming

Three conceptual domains, named consistently:

- **policy** — the user-set rule. Rename `yolo_dentry_info.perm` → `policy`
  (distinct from the resolved `cached_access` now living on the same struct).
- **access** — the resolved decision (never `HIDE`): `cached_access` /
  `cached_gen`, populated by `yolo_access_get` / `yolo_access_refresh`.
- **visibility** — `yolo_dentry_hidden`.

Not renamed (no-split decision): `enum yolo_perm` and `YOLO_PERM_*` stay,
`yolo_perm_walk` stays (used verbatim by `RULE_RESOLVE`), and
`yolo_perm_check`/`yolo_perm_check_dentry` keep their names. Only the `policy`
field and `access_get`/`access_refresh` renames above apply.

## Part C — `yolo_permission` becomes HIDE-dir-only + delegate

Make `yolo_open` the single authority for regular-file access; strip the
duplicated static decision out of `yolo_permission`:

- regular files: delete the `switch` — delegate to `inode_permission(lower)`.
  Static deny/read-only is then enforced once, in `yolo_open` via
  `yolo_perm_check_dentry`, with the exact dentry.
- directories: `yolo_dentry_hidden` → ENOENT (traversal), else delegate.
- this removes the last `d_find_alias` use and the inode-only access read, so
  Part B's dentry cache is never consulted without a dentry in hand.

Behavior change to document: `access(2)`/`faccessat(2)` on a *regular* file no
longer reflects `read-only`/`deny` (it reports the lower/mode answer); `open`,
`exec`, `truncate` stay fully gated. Covered by a new `tests/perm` case.

## Docs (first, per workflow)

Update `docs/permissions.md`:

- reframe `hide` as a visibility property — dentry-local, own-rule, no
  inheritance, no journal record — distinct from the access states;
- access resolution / cache lives on the dentry; `cached_access` is never
  `HIDE`;
- `yolo_permission` is HIDE-dir + delegate; `open` is authoritative;
- correct the "What is gated" tables and the `access(2)` semantics note;
- keep the `deny` vs `hide` anti-enumeration motivation as-is.

## Tests

`tests/perm/test_hide.rs` (extend) — behavior preserved:
- subtree hide (child stat/open/readdir ENOENT, absent from parent listing);
- cached-descendant: resolve descendant, hide ancestor live, access by full
  path → ENOENT;
- live set/unset restores visibility;
- hardlink out of a hidden dir remains reachable by its non-hidden name;
- `RULE_RESOLVE` on a hidden path still reports `hide`.

`tests/perm` (new) — `access(2)` on a read-only regular file no longer returns
EACCES; `open`/`truncate` still do.

`tests/internals/test_journal_notes.rs` — setting/unsetting a `hide` rule emits
no `C` record; other policies still do.

Hardlink divergent-rule access resolves per-name (Part B), if feasible in the
harness.

## Steps

1. `docs/permissions.md`.
2. Part A: `yolo_dentry_hidden`, repoint six surfaces, drop hide journaling.
3. Part B: migrate cache inode→dentry.
4. Part C: thin `yolo_permission`.
5. Extend/adds tests.
6. `make test`.
7. Code Review (all five categories, parallel sub-agents) before finalizing.
