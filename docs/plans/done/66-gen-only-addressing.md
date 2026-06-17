# 66 — Gen-only marker addressing across the CLI

## Goal

Make the **generation id** the sole way to address a marker everywhere in the
CLI. Today `review`/`journal` already reject names (their `parse_range` guards
endpoints with `numeric()`), but `yolo travel` still accepts a snapshot *name*
via `MarkerIndex::find_marker` → `find_snapshot_by_name`. Remove that last
name-resolution path so the whole CLI speaks gens.

Name lookup is also a footgun: snapshot names repeat (the `yolo run`
auto-snapshot labels every snapshot `after <cmd>`, and the manual default is a
timestamp), and `find_snapshot_by_name` silently returns the *last* match.
Travel markers have no name at all, so names could only ever address a subset of
valid targets. `yolo timeline` already prints the gen next to every entry, so
the copy-the-number workflow is unaffected.

## Decision

- **Gens only** for `travel`, `review`, `journal` (the latter two already are).
- Snapshot **names stay**, but only as human-readable *labels*: shown in
  `timeline`, `journal`, and success lines. They are never looked up.
  `yolo snapshot [name]` is unchanged (optional label, timestamp default).
- The base is `0` (its `(initial)` label is display-only, no longer resolvable).

## Code changes

The win here is a layering cleanup: once names are gone, **the marker/journal
layer no longer parses strings at all** — it speaks `u64` gens, and the CLI
layer owns string→gen parsing. Three functions collapse to one, and review's
hand-rolled `numeric()` digit check disappears.

1. **user/journal/marker.rs**
   - Delete `find_marker`, `find_marker_by_gen_id`, **and**
     `find_snapshot_by_name`. Replace with one bounds-check that speaks numbers:
     `resolve_gen(&self, gen: u64) -> Result<u64>` (returns `gen` if
     `< len()`, else `generation {gen} does not exist`).
   - `segment_range` takes `Option<u64>` (was `Option<&str>`) and calls
     `resolve_gen`. No more string handling in this layer.
   - Drop the `find_snapshot_by_name` unit tests and the name assertions
     (`find_marker("c1")`, the `(initial)` cases) from the gen tests.

2. **user/cmd/review.rs** — `parse_range` parses endpoints with
   `str::parse::<u64>()` directly (empty → open end), dropping the `numeric()`
   closure, and passes `u64`s to `segment_range`. Tidy the stale comment that
   implies name resolution lives in the marker layer.

3. **user/cmd/travel.rs** — `run(gen_arg: &str)` parses the arg to `u64` (friendly
   `"{arg}" is not a generation id (see \`yolo timeline\`)` on non-numeric) then
   `resolve_gen`; update the file header (`<name|id>` → `<gen>`).

4. **user/main.rs** — rename `Command::Travel { name }` field to `gen`; doc
   comment → `Snapshot/travel generation id (see \`yolo timeline\`)`.

5. **user/cmd/exec.rs** — drop "travel-by-name" from the name-purpose comment
   (names are now display-only labels).

### Considered, not doing

- **Dropping snapshot names entirely** (the `name` field on `Marker::Snapshot`
  and the `P\0<name>` journal record). Rejected: the `yolo run` auto-label
  `after <cmd>` and manual labels make `timeline`/`journal` readable. Names stay
  as labels; only their *lookup* is removed.

## Docs

- **docs/cli.md** — `yolo travel <name|gen>` → `yolo travel <gen>`; adjust prose
  so names are described as labels, not addresses.
- **docs/staging.md** — `yolo travel <name|gen>` / "named marker" → gen wording.

## Tests

- Convert every `yolo travel "<name>"` call site (~85 across `tests/cli`,
  `tests/fs`, `tests/internals`) to the corresponding gen id. Both snapshots
  **and** travels increment the gen, so compute each from its test's marker
  sequence — not a blind `"chk1"` → `"1"`.
- Error-path tests that must reach the kernel (`test_ioctl_errors` `v1`,
  `failed_travel_injection` `s1`) must use real gens.
- `travel_nonexistent_fails` → travel to an out-of-range gen (`"99"`); add/keep
  a case asserting a non-numeric arg errors at parse time.
- Unit tests in `marker.rs` updated as above.

## Verify

- `make test-vm`
- Full Code Review (parallel sub-agents per AGENTS.md).
