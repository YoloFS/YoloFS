# 38 — Flatten DirNode & Drop dtype

## Problem

`DirNode` is an enum (`File(Target)` | `Dir(Target, DirTree)`) but the
distinction is redundant — it's encoded by whether children is empty. Every
consumer pattern-matches both variants to do the same thing + optionally
recurse. Also, `dtype` in `Action` exists only to tell the tree builder
File vs Dir, which becomes unnecessary with the flattened struct.

## Changes

### 1. Flatten DirNode

```rust
// Before
pub enum DirNode {
    File(Target),
    Dir(Target, DirTree),
}

// After
pub struct DirNode {
    pub target: Target,
    pub children: DirTree,
}
```

### 2. Drop dtype from Action

```rust
// Before
Action::Stage { path, dtype: Option<u8>, ino }
Action::Delete { path, dtype: Option<u8> }
Action::Rename { src, dst, dtype: Option<u8> }

// After
Action::Stage { path, ino }
Action::Delete { path }
Action::Rename { src, dst }
```

### 3. Update tree.rs apply logic

- `apply()`: no more `is_dir` check — just set target, children stay
- `set_target()`: just set `node.target`, no File/Dir branching
- `apply_delete()`: set `node.target = Tombstone`
- `apply_rename()`: detach node (target + children move together)
- `walk_or_create_parent()`: create `DirNode { target: Passthrough, children: DirTree::new() }` for scaffolds
- If a File needs promotion to Dir (child path arrives), it just works —
  children map is always there, just empty
- `serialize()`: emit `node.children.nodes.len()` (0 for leaves)
- `for_each()`: visit target if non-passthrough, recurse into children
- `len()`: count non-passthrough targets + recurse

### 4. Update consumers

- `plan.rs`: match `node.target` instead of `DirNode::File(t)` / `DirNode::Dir(t, sub)`
- `diff.rs`: uses `for_each` — no DirNode matches
- `restore.rs`: uses tree methods — no DirNode matches
- `commit.rs`: uses plan — no DirNode matches
- `tests/`: update all test assertions
- `parse.rs`: drop dtype parsing... actually dtype is in the journal wire format from the kernel. The parser still reads it from the journal file but discards it when constructing Action.

### 5. Journal parser

Keep parsing dtype from the journal file (kernel still emits it) but discard
it — Action no longer stores it. The kernel change to stop emitting dtype
is a separate future step.

### Files touched

- `user/journal/types.rs` — Action (drop dtype), DirNode not here
- `user/journal/tree.rs` — DirNode struct, all methods, all tests
- `user/journal/plan.rs` — collect() pattern matches
- `user/journal/parse.rs` — parse dtype but discard
- `user/cmd/diff.rs` — may reference DirNode in some places
- `user/cmd/commit.rs` — no DirNode references
- `tests/internals/test_consistency.rs` — DirNode matches
- `tests/fs/test_rename.rs` — no DirNode matches (uses for_each)
