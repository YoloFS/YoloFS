# Plan: Journal Replay Redesign

## Problem

`apply_changes` in commit.rs is fragile because the resolver outputs an
**unordered** `Changeset` (internally a `HashMap` collected into a `Vec`
with arbitrary iteration order), but
applying changes to the base filesystem requires **correct ordering**:

- Rename sources with parent-child relationships must be processed
  children-first (a parent `fs::rename` silently moves all children).
- Cyclic renames (swaps) need temporary staging to break the cycle.
- The `Renamed`/`Replaced` variants carry no inode — they reference base
  paths that can become stale as other renames execute.

The current code rediscovers these constraints in `apply_changes` via
sorting, conflict detection, and temp-file insertion.  This is complex,
hard to test, and has had multiple bugs.

### Root cause

The kernel resolves redirect chains when emitting journal records.
When `tmp` is a redirect to `a`, `mv tmp b` emits `P(b, base=a)` instead
of `P(b, base=tmp)`.  This makes the `base` field refer to the
**original** base location rather than the **current physical** location,
so journal records cannot be replayed in temporal order on the base
filesystem.

## Approach

Two changes, one in the kernel and one in userspace.

### 1. Kernel: emit dentry path, fuse D+R/P into single record

In `agfs_rename`, two changes:

**a) Stop resolving redirect chains** — always use `old_buf` (the
dentry-relative path) for the base field:

```c
// Before:
redirect_path = redirect ? redirect : old_buf;

// After:  (just delete the redirect lookup)
redirect_path = old_buf;
```

**b) Fuse `D` + `R`/`P` into a single record.**  The kernel already
emits them as an atomic pair (line 195-216 of inode.c).  New journal
format for renames:

```
R\0<old_dir>\0<old_name>\0<new_dir>\0<new_name>\0<dtype>\n   # rename to new path
P\0<old_dir>\0<old_name>\0<new_dir>\0<new_name>\0<dtype>\n   # rename to existing path
```

Each rename record is now self-contained (old path + new path).  No
consumer needs to know about the D+R/P pairing convention.  Non-rename
`D` records are unchanged.

`A`/`M`/`D`/`K`/`S` records are unchanged.

### 2. Userspace: simplify, then collapse

Replace the current single-stage resolver with a two-view pipeline:

```
journal records  →  simplify()  →  ActionList    (ordered, for commit)
                                       ↓
                                   .collapse()  →  Changeset   (for diff/restore/abort)
```

K/S records are already stripped by segmentation, so `simplify()` and
`collapse()` only see A/M/D/R/P records.  After fusing D+R/P in the
kernel, R/P records are self-contained (old + new path).

`simplify()` outputs `ActionList` — a newtype over `Vec<Action>` with
methods for applying and collapsing.  `Action` is a separate type from
`Record` with only the 5 operation variants (no K/S).  The type system
enforces that checkpoint/restore markers can't leak into the replay
pipeline.

```rust
// cli/journal/types.rs
enum Action {
    Add { path: String, ino: u64, dtype: DType },
    Modify { path: String, ino: u64, dtype: DType },
    Delete { path: String },
    Rename { old: String, new: String, dtype: DType },
    Replace { old: String, new: String, dtype: DType },
}

// cli/journal/action.rs
pub struct ActionList(Vec<Action>);

impl ActionList {
    /// Apply actions sequentially to the base filesystem.
    pub fn apply(&self, agfs: &Path) -> Result<()> { ... }

    /// Derive the state summary for display commands.
    pub fn collapse(&self) -> Changeset { ... }
}
```

**`simplify(records) → ActionList`** — produces a shorter, ordered
sequence that is directly replayable on the base filesystem:

- **Chain collapse**: `R(a, b) + R(b, c)` → `R(a, c)`.  Placed at the
  position of the first record in the chain.  Multi-step chains
  (`a→b→c→d`) are handled by iterating: build a map from source to
  record, follow dest→source links to find the chain head, then
  rewrite the head record's dest to the chain tail's dest and remove
  intermediate records.  **Skip chains that would form a cycle.**  Build a
  `HashMap<src, dest>` from all renames.  For each chain head, follow
  dest→src links; if you revisit a node the chain is cyclic — leave
  those records uncollapsed.  Example: swap `R(a, tmp) + R(b, a) +
  R(tmp, b)` — the chain `a→tmp→b` has dest `b`, which is itself a
  source pointing back through the graph to `a`.  Collapsing would
  produce `R(a, b) + R(b, a)`, which can't be replayed sequentially.
  Uncollapsed records replay correctly because the intermediates are
  created and moved on base.
- **Cancel**: `A(x, ino) + D(x)` → removed.  `M(x, ino) + D(x)` →
  `D(x)` (the modify is redundant when the file is deleted).  This
  also handles staged file renames: the kernel emits `D + A/M` (not
  `D + R/P`) for staged sources, so `A(old) + D(old)` from a rename
  cancels, leaving only the `A/M(new)`.
- **Merge modifies**: `M(x, ino=1) + M(x, ino=2)` → `M(x, ino=2)`.
- **Decompose rename+modify**: `R(a, b) + M(b, ino)` → `D(a) + A(b, ino)`
  (b is new to base since the original was R, so the action must be A
  for collapse to produce `Change::Added`).
  `P(a, b) + M(b, ino)` → `D(a) + M(b, ino)` (b existed in base, so
  M is correct).
- Everything else: kept in temporal order.
- **Rule ordering**: decompose rename+modify first (prevents chain
  collapse across intervening modifications), then chain collapse (skip
  cycles), then cancel, then merge modifies.

Because the kernel now emits dentry paths (current mount paths), the
temporal order is the correct replay order for the base filesystem.
No sorting by source path, no temp-file insertion, no conflict
detection — those were artifacts of the kernel's redirect resolution.

The only caveat is that chain collapse is still needed because the chain
intermediate does not exist in base (e.g. `mv a tmp; mv tmp b` — `tmp`
never existed in base, so replaying `R(a, tmp) + R(tmp, b)` would
create `tmp` and then move it, which works but is unnecessary).

**`ActionList::collapse() → Changeset`** — derives the state summary for
display commands.  Iterate the actions and accumulate net
effects into a HashMap.  `Rename{old, new}` → `Change::Renamed{from: old}`
at `new` + `Change::Deleted` at `old`.  `Replace{old, new}` does the
same but also handles the overwritten destination: if `new` already
has a prior `Renamed`/`Replaced` entry, re-insert `Change::Deleted` for
that entry's origin; otherwise the overwrite is implicit (base file at
`new` is replaced in-place).  The Added/Modified and Renamed/Replaced
distinction maps directly to the `Change` enum.

**`ActionList::apply()`** — trivial sequential replay on base:

```rust
for action in &self.0 {
    match action {
        Add { path, ino, .. } | Modify { path, ino, .. } => {
            write_staged_inode(ino, path);
        }
        Delete { path }              => remove(path),
        Rename { old, new, .. }
        | Replace { old, new, .. }   => fs::rename(old, new),
    }
}
```

No temp files.  No ordering logic.  No conflict detection.

## Changes

### Kernel (`kmod/`)

| ID | Task | Files |
|----|------|-------|
| kmod-dentry-path | Stop resolving redirect chain in `agfs_rename`: always use `old_buf` for the journal base field. Remove the `redirect`/`redirect_path` logic. Rename `in_base` to `overwrites` on `agfs_dirent` and in comments (semantic clarification, no behavior change). | `kmod/inode.c`, `kmod/agfs.h`, `kmod/staging.c` |
| kmod-fuse-records | Fuse `D` + `R`/`P` into a single rename record carrying both old and new paths. Update `agfs_journal_redirect` and `agfs_journal_replace` to accept old path. Remove the separate `agfs_journal_delete` call for renames. | `kmod/inode.c`, `kmod/journal.c` |

### Userspace (`cli/`)

| ID | Task | Files |
|----|------|-------|
| parse-new-format | Update journal parser to handle new `R`/`P` format with old+new paths. Update `Record` enum: `Redirect` and `Replace` carry both `old` and `new` path instead of `path` + `base`. | `cli/journal/parse.rs`, `cli/journal/types.rs` |
| simplify | Implement `simplify(records) → ActionList`: chain collapse (skip cycles), cancellation, merge modifies, rename+modify decomposition. Define `Action` enum and `ActionList` newtype. Output is ordered and directly replayable. | `cli/journal/types.rs`, `cli/journal/simplify.rs` (new) |
| action-impl | Implement `ActionList::apply()` and `ActionList::collapse()`. | `cli/journal/action.rs` (new) |
| wire-resolve | Rewrite the public `resolve()` API to call `simplify().collapse()`. Remove the old `Resolver` struct and its spurious-delete handling (line 84 of current resolve.rs). | `cli/journal/resolve.rs` (rewrite) |
| apply-rewrite | Rewrite `apply_changes` in commit.rs to call `ActionList::apply()`. Remove all temp-file, sorting, and conflict-detection logic. | `cli/commit.rs` |
| migrate-callers | Update all CLI commands to use `simplify()` + `ActionList::collapse()` instead of `resolve()`. | `cli/{diff,restore,abort,mount}.rs` |
| update-docs | Update `docs/staging.md` (journal format, rename handling) and `docs/architecture.md`. | `docs/` |
| unit-tests | Rewrite the 58 existing resolver unit tests for the new `simplify()` + `collapse()` pipeline. Add new unit tests: cycle skip (2-cycle swap, 3-cycle rotation), multi-step chain collapse, interleaved chains, staged rename cancel, replace overwrite tracking. | `cli/journal/simplify.rs`, `cli/journal/action.rs` |
| e2e-tests | Update `tests/internals/helpers.rs` (`changes()` helper calls `resolve()` → switch to `simplify().collapse()`). Update any e2e tests that inspect raw journal records for the new R/P format. | `tests/internals/helpers.rs`, `tests/` |

### Dependencies

```
kmod-dentry-path ─→ kmod-fuse-records ─→ parse-new-format ─→ simplify
                                                                  ↓
                                                             action-impl
                                                                  ↓
                                                         ┌────────┴────────┐
                                                         ↓                 ↓
                                                      wire-resolve        apply-rewrite
                                                         ↓                 ↓
                                                   migrate-callers ←───────┘
                                                         ↓
                                               ┌─────────┴─────────┐
                                               ↓                   ↓
                                          update-docs        unit-tests
                                                                   ↓
                                                               e2e-tests
```

## Notes

- The `R`/`P` distinction (dest overwrites existing content vs new path)
  is kept in the journal format.  `collapse()` uses it to produce
  `Renamed` vs `Replaced` in the Changeset.  `simplify()` and `apply()`
  ignore it.

- `simplify()` is pure: it takes `Vec<Record>` and returns `ActionList`.
  No filesystem access.  Fully unit-testable.  `Action` is a separate
  type from `Record` — 5 operation variants only, no K/S.
  `ActionList` is a newtype over `Vec<Action>` with `apply()` and
  `collapse()` methods, living in `cli/journal/action.rs`.

- The journal wire format changes only for `R`/`P` records: they now
  carry `<old_dir>\0<old_name>\0<new_dir>\0<new_name>\0<dtype>` instead
  of `<dir>\0<name>\0<dtype>\0<base>`.  Each rename record is
  self-contained — no D+R/P pairing convention for consumers to know.
  `A`/`M`/`D`/`K`/`S` are unchanged.

- The kernel changes are small: remove redirect resolution (~5 lines),
  update journal_redirect/journal_replace to accept old path (~20 lines),
  remove the separate journal_delete call for renames (~2 lines).

- Additional simplifications considered but deferred: `R(a, x) + D(x)`
  → `D(a)` (rename then delete = delete original), `M(x, ino) + R(x, y)`
  → `D(x) + A(y, ino)` (modify then rename).  Both are correct without
  simplification (apply replays them sequentially), so they are
  optimizations, not correctness fixes.  Add later if `collapse()` needs
  cleaner input.

- For backwards compatibility: none.  Old journals are invalid after
  the format change.  Commit or abort before upgrading.
