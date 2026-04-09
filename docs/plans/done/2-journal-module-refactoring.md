# Journal Module Refactoring & CLI Command Rename

## Problem

The journal-related code is scattered across `journal.rs`, `resolve.rs`,
`checkpoint.rs`, and `audit.rs` with duplicated logic (e.g. `reachable()` vs
`reachable_indices()`). The CLI commands (`log`, `tree`, `audit`) don't match
the internal abstractions, forcing users to learn two naming systems.

## Design

### Three abstractions, three levels

```
journal::parse::read()  →  Timeline::new()  →  timeline.resolve()
    (raw records)           (segments +          (collapsed final
                             reachability)        changes per path)
```

| Level | Internal type | CLI command | What it shows |
|-------|--------------|-------------|---------------|
| Raw records | `Vec<Record>` | `yolofs journal` | Every ADD/MOD/DEL/RDR/REP/CKP/RST record |
| Structured segments | `Timeline` | `yolo timeline` | Checkpoints + restores (DAG, unreachable dimmed) |
| Collapsed changes | `Vec<Change>` | `yolo status` / `yolo diff` | Final effect per path |

### Module layout

```
cli/
├── journal/
│   ├── mod.rs          # pub use re-exports
│   ├── types.rs        # Record, Checkpoint, DType, INO_REDIRECT
│   ├── parse.rs        # read(), inode_path(), truncate()
│   ├── timeline.rs     # Timeline, Segment
│   └── resolve.rs      # Resolver, Change, ResolvedSegment
│
├── checkpoint.rs       # yolo checkpoint (create only)
├── journal_cmd.rs      # yolofs journal (raw record display)
├── timeline_cmd.rs     # yolo timeline (DAG display)
├── diff.rs             # yolo status / yolo diff (resolved changes)
├── commit.rs           # yolo commit
├── restore.rs          # yolo restore
├── ...                 # other commands unchanged
```

### Segment as the core type

```rust
pub struct Segment {
    pub from: Checkpoint,
    pub to: Option<Checkpoint>,   // None = trailing unsaved changes
    pub reachable: bool,
    pub records: Vec<Record>,     // raw ADD/MOD/DEL/RDR/REP records in this segment
}

pub struct Timeline {
    pub segments: Vec<Segment>,
}

impl Timeline {
    /// Build from raw journal records. Computes segment boundaries (CKP/RST)
    /// and reachability.
    pub fn new(records: Vec<Record>) -> Self;

    /// Iterate reachable segments only.
    pub fn reachable(&self) -> impl Iterator<Item = &Segment>;

    /// Find a checkpoint by name or gen_id.
    /// Searches ALL segments (including unreachable) for restore targets.
    pub fn find_checkpoint(&self, name_or_id: &str) -> Result<&Checkpoint>;

    /// Slice to a checkpoint range (--at, --from, --to).
    /// Only searches reachable segments.
    pub fn slice(&self, at: Option<&str>, from: Option<&str>, to: Option<&str>)
        -> Result<Vec<&Segment>>;

    /// Resolve all reachable records into collapsed changes.
    pub fn resolve(&self) -> Result<Vec<Change>>;

    /// Resolve reachable records into per-segment resolved changes.
    pub fn resolve_segments(&self) -> Result<Vec<ResolvedSegment>>;
}
```

### CLI command mapping

| Current | New | Notes |
|---------|-----|-------|
| `yolofs log` | *(removed)* | Merged into `yolo timeline` |
| `yolofs tree` | *(removed)* | Merged into `yolo timeline` |
| `yolo audit` | `yolofs journal` | Shows every record, unreachable dimmed |
| `yolo audit --path X` | `yolofs journal --path X` | Filter to one file |
| *(new)* | `yolo timeline` | Shows K+S events, unreachable dimmed |
| `yolo status` | `yolo status` | Unchanged (resolved) |
| `yolo diff` | `yolo diff` | Unchanged (resolved) |

## Todos

### Phase 1: Create journal module

1. **journal-types** — Create `cli/journal/types.rs` with `Record`,
   `Checkpoint`, `DType`, `INO_REDIRECT`. These are pure data types
   with no I/O.

2. **journal-parse** — Create `cli/journal/parse.rs` with `read()`,
   `inode_path()`, `truncate()`. These are the I/O functions that
   operate on `.yolofs/` files.

3. **journal-mod** — Create `cli/journal/mod.rs` that re-exports
   everything from types.rs and parse.rs. Delete old `cli/journal.rs`.
   All existing `use crate::journal` imports continue to work.

4. **journal-timeline** — Create `cli/journal/timeline.rs` with
   `Timeline` and `Segment`. Move logic from `resolve.rs`
   (`reachable()`, `find_checkpoint_index()`, `slice_records()`) and
   `audit.rs` (`reachable_indices()`) into `Timeline` methods.

5. **journal-resolve** — Move `Resolver`, `Action`, `Change`,
   `resolve()`, `resolve_segments()` from `cli/resolve.rs` into
   `cli/journal/resolve.rs`. Rename the current `Segment` output type
   to `ResolvedSegment`. Add `Timeline::resolve()` and
   `Timeline::resolve_segments()` as convenience methods.
   Delete `cli/resolve.rs`.

### Phase 2: Update commands & consumers

6. **cmd-journal** — Rename `cli/audit.rs` → `cli/journal_cmd.rs`.
   Change `Command::Audit` → `Command::Journal` in main.rs.
   Update to use `Timeline` for reachability.

7. **cmd-timeline** — Create `cli/timeline_cmd.rs`. Merge
   `checkpoint::log()` + `checkpoint::tree()` into a single
   `timeline_cmd::run()`. Change `Command::Log` + `Command::Tree` →
   `Command::Timeline` in main.rs. `checkpoint.rs` keeps only
   `create()`.

8. **update-commit** — Update `cli/commit.rs` to use
   `Timeline::resolve()`.

9. **update-diff** — Update `cli/diff.rs` to use
   `Timeline::slice()` + `Timeline::resolve_segments()`.

10. **update-restore** — Update `cli/restore.rs` to use
    `Timeline::find_checkpoint()` + resolve on reachable prefix.

### Phase 3: Docs & tests

11. **update-docs** — Update docs/cli.md, docs/staging.md,
    docs/architecture.md for new command names and module structure.

12. **update-tests** — Update E2E tests: `s.cli(&["audit"])` →
    `s.cli(&["journal"])`, `s.cli(&["log"])` / `s.cli(&["tree"])` →
    `s.cli(&["timeline"])`.

13. **move-tests** — Move unit tests from old resolve.rs and journal.rs
    into their new homes under `journal/`.

## Notes

- `yolo checkpoint` (create) stays in checkpoint.rs — it's a command,
  not a journal query.
- `Segment` (timeline.rs) holds raw records + reachable flag.
  `ResolvedSegment` (resolve.rs) holds collapsed `Vec<Change>`.
- `Timeline::new()` replaces the scattered `reachable()` +
  `resolve_segments()` setup — all consumers start with a Timeline.
- `--path` filter on `yolofs journal` stays — it's a query on raw records.
