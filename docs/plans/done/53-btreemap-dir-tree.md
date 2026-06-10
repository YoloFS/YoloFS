# 53 — Deterministic dir-tree ordering (`HashMap` → `BTreeMap`)

## Problem

`DirTree.nodes` is a `HashMap<String, DirNode>` (`user/journal/tree.rs:35`).
Rust randomizes HashMap iteration order per process, and `visit_targets`
(`tree.rs:400`) iterates it directly, so every consumer of `for_each`
inherits a random order:

- `yolo review` / `review all` / `--diff` / `--each` list changes in a
  different order on every invocation (observed: `step1.txt`/`step2.txt`
  swapping places in `example.out` across regens, so `example.out` churns
  on every `./example.sh` run).
- The commit plan (`user/journal/plan.rs:96` iterates `tree.nodes`) has
  nondeterministic sibling op order — harmless for correctness, since
  parent-before-child comes from the DFS structure, but the committed
  journal bytes differ run to run.
- `serialize_into` (`tree.rs:170`) already papers over it with a
  collect+sort per directory, because the travel ioctl wire format needs
  deterministic bytes.

Journal (chronological) order was considered and rejected: it is
ill-defined after net-effect collapsing (renames move subtrees; a path can
be touched many times), and it is still nondeterministic for parallel
workloads — `yolo run -- make -j8` touches files in scheduler order, so the
churn would remain. Path order is the only ordering that is a pure function
of the final state, and it matches `git status` conventions.

## Design

Swap the map type:

```rust
pub nodes: BTreeMap<String, DirNode>,
```

Iteration becomes byte-lexicographic per path component, depth-first —
deterministic by construction for all three consumers (review listing,
commit plan, travel serialization). Per-component lookup goes from O(1)
hash to O(log fanout) string compares; at this tree's scale (bounded by the
staged changes of one run, in a short-lived CLI process) the difference is
unmeasurable, and determinism-by-construction kills the
"forgot-to-sort-a-new-traversal" bug class permanently.

Fallout, all mechanical:

- `serialize_into` becomes a direct streaming loop: drop both the
  collect-and-sort (BTreeMap iteration is already name-sorted) and the
  skip-empty-passthrough filter. The filter is what forced the collect —
  `child_count` is written before the children, and with the filter the
  count differs from `nodes.len()`. Without it the count IS `nodes.len()`,
  known up front, so no intermediate Vec at all. Dropping the filter is
  safe on both sides:
  - The kernel already parses an empty passthrough entry (tag=2,
    `path_len=0`, `child_count=0`) as a pure no-op — `travel_inject_entry`
    returns without creating state (`kmod/ioctl.c:506`) and `child_count=0`
    skips descent (`kmod/ioctl.c:608`). No kernel change.
  - `DirTree::build` never produces empty passthrough scaffolds (every
    path that detaches a subtree immediately plants a tombstone back at
    the vacated position), so emitted bytes are unchanged in practice;
    only hand-built trees (unit tests) can observe the difference.

  Wire format is unchanged (`docs/staging.md` travel step 4, "children
  sorted by name", stays true; staging.md never documented the
  empty-scaffold omission, so no doc change there either).
- All methods called on `nodes` — `get` / `get_mut` / `insert` / `remove`
  / `contains_key` / `is_empty` / `iter` / `values` / by-ref iteration —
  have identical signatures on BTreeMap, so call sites in `tree.rs`,
  `plan.rs`, and tests compile unchanged. The derives
  (`Debug, Clone, PartialEq, Default`) all hold.
- `user/changeset.rs` needs no change: its `preimage` HashMap is
  lookup-only (never iterated for output).

## Steps

1. **Docs first.** `docs/cli.md`, output-contract section (~line 121): state
   that review/diff listings are path-sorted and stable across runs.
   `docs/staging.md` already describes sorted serialization — no change.
2. **Failing tests** (this is a bug fix; do not touch existing tests):
   - Unit test in `tree.rs`: build a tree from ~10 stages in non-sorted
     insert order, collect `for_each` paths, assert equality against an
     explicit expected vector. Under HashMap this fails with probability
     1 − 1/10!.
   - E2E test in `tests/cli/test_status.rs`: stage ~10 files in one
     `yolo run`, extract the path lines from `yolo review` output, assert
     they equal the expected list.

   Assert exact expected order, NOT `is_sorted()` on full path strings:
   per-component DFS order differs from flat string order when a name
   shares a prefix with a sibling directory (`/a.txt` < `/a/b` as strings
   since `.` < `/`, but DFS emits `/a/b` first), so an `is_sorted` check
   would be wrong — or silently flaky depending on the chosen filenames.
   Run both to confirm they fail before the fix.
3. **Implement** in `user/journal/tree.rs`: change the import (line 15),
   the field type (line 35), and `new()` (line 41); rewrite
   `serialize_into` as the streaming loop above (write
   `nodes.len()` as the count, then iterate). No code changes elsewhere.
4. Existing tests pass untouched, with one deliberate exception:
   `serialize_passthrough_dir_empty_subtree_omitted` asserts the dropped
   filter. This is a behavior change, not a bug fix, so updating it is
   in-scope: rename it to `serialize_empty_passthrough_dir_emitted` and
   assert the new bytes (child_count=1, name, tag=2, path_len=0,
   child_count=0). Every other `serialize_*` test already asserts sorted
   output, and direct `tree.nodes` accesses in tests are API-compatible.
5. **Regenerate `example.out`** (`./example.sh`): the `review all` /
   `review all --diff` stanzas settle into sorted order, ending the churn.
6. `make test-vm`.
7. **Code review** per AGENTS.md (parallel sub-agents over the diff), then
   move this plan to `docs/plans/done/`.
