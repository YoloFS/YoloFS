# 51 — Positional rev/range grammar for status & diff

## Motivation

`status`/`diff` accreted four flags — `--at`, `--from`, `--to`, and `--full`
(just renamed `--base`). They conflate two independent axes and the names mislead
(`--full` promised "more" but vs-base can show *less*). Collapse the whole surface
to a git-style positional revision/range plus one granularity flag.

## The two axes

Every view is a point in `(range) × (granularity)`:

- **range** `[start, end)` — which segments.
- **granularity** — one **net** diff for the whole range (default), or one diff
  **per consecutive snapshot** in it (`--each`).

## Grammar (both `status` and `diff`)

```
yolo status [<rev>[..<rev>]] [--each]
yolo diff   [<id>[..<id>]] [--each] [-- <path>]
```

Ids are **numbers only** (`0` is the base/"(initial)" marker — internally a
journal `gen_id`, but user-facing it's an *id*). `diff`'s optional path filter is
passed after `--` (git pathspec), so the positional is unambiguously a range — no
token heuristic. `status` has no path (filtering a one-line summary is pointless).

Naming: **id** = a snapshot (user-facing, what `snapshot [N]` prints); **range** =
a span; **gen_id** = the internal/on-disk counter (also names travels). "rev" and
"base" were dropped as redundant.

| Command | Range | Meaning |
|---|---|---|
| `yolo diff` | latest segment | vs prev *(default)* |
| `yolo diff 7` | `prev(7)..7` | snapshot 7's own change |
| `yolo diff 3..5` | `[3,5)` | state(3) → state(5), net |
| `yolo diff 3..` | `[3,tip)` | since 3 |
| `yolo diff ..5` | `[0,5)` | base → 5 |
| `yolo diff ..` (or `0..`) | `[0,tip)` | everything vs base (what `commit` applies) |
| `yolo diff 3..5 -- foo.txt` | `[3,5)` | range, limited to `foo.txt` |
| `… --each` | per segment | one diff per consecutive snapshot |

Edge cases (all fall out of the grammar, no special-casing):
- bare `0` → `prev(0)..0` = empty range → "(no changes)" (the base has no change
  of its own). vs-base is the *range* `..` / `0..`.
- `--each` with **no** spec → whole session (`..`) per-step (≈ timeline+diffs);
  with a spec → that range per-step; with a bare `N` → one step (= `diff N`).

## Efficiency (already established)

`Changeset::collect(start, end)` loops only `start..end`, uses each path's
first-touch pre-image as the old side (no `[0,start)` replay), and builds the net
tree from the range's live segments — **O(records in range)**. Default and single-
snapshot stay O(1 segment); ranges scale with the span; `--each` is the same total
as net (each record once), just grouped; only `..` is O(journal), inherently.
The journal *parse* is the only O(journal) floor, shared by every command today.

## Changes

1. **journal/tree.rs** — `DirTree::build` borrows: generic over `Borrow<Segment>`
   (owned *or* `&Segment`), `apply` takes `&Action`. Lets `collect` build a tree
   without consuming the journal — needed to call it per-segment for `--each`.
   Owned callers (commit/travel/mount/abort) are unchanged.
2. **journal/core.rs** — add a borrowing `live_segments_range(&self)` alongside the
   owned `into_live_segments_range`.
3. **changeset.rs** — `collect(journal: &Journal, …)` borrows, building the tree
   from `journal.live_segments_range(start, end)` (no clone, journal not consumed).
4. **cmd/diff.rs**
   - `resolve_id(tok, journal)` (numbers only) + `parse_range(spec, each, journal)`:
     bare `N` → `prev(N)..N`; `a..b` → range with empty ends = base/tip; none →
     latest (or `..` when `--each`).
   - `run_status(range, each)`, `run_diff(range, path, each)`; drop `at/from/to/base`.
     No id-vs-path heuristic — path comes after `--`.
   - `--each`: `render_each` walks live, non-empty segments in `[start,end)`,
     rendering `snapshot [i+1]` + that segment's summary (status) / unified diff.
   - `classified()` shared projection ("what shows") for summary + `--each` gating.
   - Default-view hint points at `..` (vs base) and `--each`.
5. **main.rs** — `Status { range, each }`, `Diff { range, each, path }` (path is a
   `last = true` positional, i.e. after `--`). Drop `at/from/to/base`. `audit --full`
   is untouched (it means "full history").
6. **Tests** — migrate `--at/--from/--to/--base` → positional ids in
   test_status.rs, test_diff.rs, test_snapshot.rs, test_travel.rs (snapshots map to
   gen ids: first `snapshot` = 1, etc.; travels bump the id). Path filter →
   `-- <path>`. Drop the obsolete clap-conflict test; add `--each`, bare-`N`, `..`,
   and backwards-range-error coverage. `test_audit.rs` unchanged.

## Out of scope
- `commit` preview (the natural home for vs-base) — separate change.
- Keeping/dropping snapshot *names* on `travel` — left as-is (names + ids).
- Cached marker byte-offset index (sub-linear parse) — only if journals get large.

## Verification
`cargo build`, `cargo test --lib`, `cargo clippy --all-targets`, then
`make test-e2e-vm`.
