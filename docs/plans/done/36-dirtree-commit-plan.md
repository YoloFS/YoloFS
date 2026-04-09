# 36 — DirTree-Based Commit

## Problem

`yolo commit` currently replays every live journal record in chronological
order. The journal `[S /a 1, D /a, S /a 2]` emits three filesystem operations
when only one is needed (stage inode 2 at `/a`). Worse, redundant renames can
fail if intermediate base state doesn't support them (e.g. renaming a base path
that a previous replay step already moved).

The fix: build a `DirTree` from the live segments (which already collapses
redundant operations), then convert the tree into a minimal **commit plan** —
an ordered list of `CommitOp`s applied to the base filesystem.

## Design

### CommitOp

```rust
enum CommitOp {
    Stage  { path: String, ino: u32 },
    Link   { dst: String, src: String },
    Delete { path: String },
}
```

No `Mkdir` — `ensure_parent` handles parent directory creation at apply time.

### Three-Phase Execution

> **Implementation note**: The original design had four phases with a
> "pre-deletes" phase for tombstoned dirs with Link descendants.  The
> implementation simplified this to three phases because a Tombstone node
> with live (non-tombstone) children is unreachable: staging a child after
> deleting its parent re-creates the parent (overwriting the Tombstone
> with StagedFile), so by construction, a Tombstone's subtree is dead.
> The collect step therefore never recurses into Tombstone nodes.

| Phase | Ops | Order within phase | Rationale |
|-------|-----|--------------------|-----------|
| 1. Renames | `Rename` | topo-sort by source/dest/prefix conflicts | Run while base is pristine — renames read from base paths |
| 2. Deletes | `Delete` | deepest-first (by path depth) | Clear old content; children before parents |
| 3. Stages | `Stage` | shallowest-first (by path depth) | Create new content; parents before children |

Why this ordering is correct:
- **Renames before Deletes/Stages**: Renames read from base. If a Delete or Stage
  writes to a rename's source path, running renames first avoids the conflict.
  E.g. `Rename(/b, /a)` + `Stage(/a, 5)`: Rename reads base/a first, then Stage
  overwrites it.
- **Deletes before Stages**: A tombstoned directory with staged descendants
  needs `remove_dir_all` to clear old base content before `ensure_parent`
  recreates the dir and stages new files.
- **Deepest-first Deletes**: Children must be gone before `rmdir` on parent.
  (In practice `remove_dir_all` handles nested content, but explicit ordering
  lets us skip redundant child deletes under a deleted parent.)
- **Shallowest-first Stages**: Parent dirs must exist before staging children.
  (`ensure_parent` also handles this, but explicit ordering is cleaner.)

### Collecting Ops from DirTree

Walk `DirTree.nodes` recursively via DFS:

| Node target | Action |
|-------------|--------|
| `StagedFile(ino)` | emit `Stage { path, ino }`, recurse into children |
| `BasePath(src)` | emit `Rename { dst: path, src }`, recurse into children |
| `Tombstone` | emit `Delete { path }`, **stop** (subtree is dead) |
| `Passthrough` | recurse into children only |

### Ordering Renames (Phase 1)

Renames may conflict in four ways:

- **Exact match**: `Rename(A).src == Rename(B).dst` → A must come before B
  (A reads from source before B overwrites it as destination).
- **Prefix (parent creates child's source)**: `Rename(B).dst` is a strict path
  prefix of `Rename(A).src` → B must come before A (B creates the directory
  that A reads from). E.g. `Rename(/c,/a)` must precede `Rename(/d,/c/file)`.
- **Prefix (parent creates child's source, reversed)**: `Rename(A).dst` is a
  strict path prefix of `Rename(B).src` → A must come before B.
- **Source prefix**: `Rename(B).src` is a strict path prefix of `Rename(A).src`
  → A must come before B (A reads a child of what B moves — the child read
  must happen before the parent rename). E.g. `Rename(/x,/dir/f)` must precede
  `Rename(/y,/dir)`.

Build a dependency graph with edges for all four rules.
Topological sort with **general cycle detection** (Kahn's algorithm).

**Cycle = rotation pattern.** Example: 3-way rotation
`Rename(/a,/c)`, `Rename(/c,/b)`, `Rename(/b,/a)` forms cycle A→B→C→A.

**Cycle breaking:**
1. Find any cycle (via Kahn's — nodes with no in-edges are emitted; remaining
   nodes form cycles).
2. Pick any Link `L` in the cycle with `src = S`.
3. Generate a temp path: `<parent_of_S>/.yolofs-commit-tmp-<n>` (same parent dir
   ensures same filesystem for `rename`).
4. Emit a preliminary `rename(base/S, base/tmp)`.
5. Rewrite `L.src = tmp_path`.
6. The cycle is broken. Re-run topo sort on remaining Links.

### Apply

- `Stage { path, ino }` → existing `apply_inode()` (stats inode, handles
  symlink/dir/file, removes existing at destination, ensure_parent).
- `Link { dst, src }` → `ensure_parent(dst)`, remove existing at `dst` if
  present (handles `ENOTEMPTY` for dirs), then `fs::rename(base/src, base/dst)`.
- `Delete { path }` → existing `remove_existing()` if path exists (no-op if
  already gone, e.g. after a Link moved it).

### Edge Cases

| Case | Example | Resolution |
|------|---------|------------|
| Swap | `Rename(/a,/b)` + `Rename(/b,/a)` | 2-cycle, broken with temp path |
| N-way rotation | `Rename(/a,/c)` + `Rename(/c,/b)` + `Rename(/b,/a)` | N-cycle, broken with temp path |
| Stage at Rename source | `Rename(/b,/a)` + `Stage(/a,5)` | Three-phase: Rename (P1) before Stage (P3) |
| Rename dst overwrites Rename src | `Rename(/a,/c)` + `Rename(/c,/b)` | Within P1: topo sort handles |
| Rename prefix dependency | `Rename(/c,/a)` + `Rename(/d,/c/file)` | Within P1: prefix topo sort handles |
| Tombstoned dir (subtree dead) | `Delete(/dir)` | P2 deletes dir; subtree not recursed (dead by construction) |
| Delete child before parent | `Delete(/d/f)` + `Delete(/d)` | P2 deepest-first, or fold child under parent |
| Parent stage before child | `Stage(/d,ino)` + `Stage(/d/f,ino2)` | P3 shallowest-first |
| Rename needs ensure_parent | `Rename(/new/path,/old)` after `/new` deleted | P1 apply does ensure_parent before rename |
| Delete after Rename moved source | `Rename(/b,/a)` + `Delete(/a)` | Delete is no-op (base/a already gone) |

## Changes

### `user/journal/plan.rs` (new)
- `CommitPlan` struct with three phase buckets: `renames`, `deletes`, `stages`.
- `fn into_plan(tree: &DirTree) -> CommitPlan`
  — walks tree recursively, populates three buckets.
  Deletes sorted deepest-first, stages sorted shallowest-first.
- `fn order_renames(renames: &mut Vec<Action>)` — topo sort + cycle breaking.
  Returns temp-rename pairs for cycles (prepended to renames bucket).
  Handles exact-match, prefix, and source-prefix deps.

### `user/cmd/commit.rs`
- `fn apply_plan(yolofs, plan) -> Result<usize>` — iterates plan fields
  directly: renames, then deletes, then stages.
- Replace `apply_records()` call in `run()` with:
  1. `Journal::read().into_tree().into_plan()`
  2. `apply_plan(&yolofs, &plan)`
- Keep existing `ensure_parent`, `apply_stage`, `remove_existing` helpers.

### `docs/staging.md`
- Update the "Commit" section to describe DirTree-based plan approach.

### Tests (unit, in `commit.rs`)
- Plan generation: simple stage, delete, rename, mixed trees.
- Tombstoned dir with all-tombstone subtree (skip children).
- Tombstoned dir with staged child (emit both).
- Link ordering: source/dest conflict.
- Swap cycle (2-way) detection and breaking.
- N-way rotation (3-way) cycle breaking.
- Delete depth ordering.
- Stage depth ordering.
