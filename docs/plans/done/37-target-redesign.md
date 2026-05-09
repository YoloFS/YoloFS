# 37 — Target Redesign & CommitOp Elimination

## Problem

`Target::Path(Option<String>)` packed two unrelated concepts into one variant:
`Path(None)` = passthrough, `Path(Some(src))` = redirect. Combined with
`Target::None` for tombstone, the naming was confusing (two `None`s meaning
different things). Redesigned to four explicit variants: `StagedFile`,
`BasePath`, `Passthrough`, `Tombstone`.

Separately, `CommitOp` in `commit.rs` duplicates `Action` — the commit
pipeline is Actions → DirTree → Actions, so the output should reuse `Action`.

## Changes

### 1. Redesign `Target` (types.rs)

```rust
// Before
pub enum Target {
    Inode(u32),
    Path(Option<String>),  // None=passthrough, Some=redirect
    None,                  // tombstone
}

// After
pub enum Target {
    StagedFile(u32),
    BasePath(String),
    Passthrough,
    Tombstone,
}
```

Helper method changes:
- `passthrough()` → remove, use `Target::Passthrough` directly
- `is_passthrough()` → remove, use `matches!(t, Target::Passthrough)`
- `ino()` → keep (no callers currently, but useful)
- `matches_path()` → update `Path(Some(src))` arm to `BasePath(src)`

### 2. Eliminate `CommitOp` (commit.rs)

`CommitOp` was proposed in Plan 36 but the implementation went directly to
using `Action`, skipping the intermediate type entirely. `CommitPlan` is not
needed — `into_actions()` returns a flat `Vec<Action>`.

Mapping (conceptual):
- `CommitOp::Stage { path, ino }` → `Action::Stage { path, ino }`
- `CommitOp::Link { dst, src }` → `Action::Rename { dst, src }`
- `CommitOp::Delete { path }` → `Action::Delete { path }`

### Files touched

- `user/journal/types.rs` — Target enum + helpers
- `user/journal/tree.rs` — DirTree builder + serialize + tests (~80 sites)
- `user/cmd/commit.rs` — CommitOp → Action, Target variant updates (~34 sites)
- `user/cmd/diff.rs` — Target variant updates (~19 sites)
- `user/cmd/restore.rs` — Target variant updates in tests (~6 sites)
- `user/cmd/audit.rs` — no Target usage, only Action (no change needed)
