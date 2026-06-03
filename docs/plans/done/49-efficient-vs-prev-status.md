# 49 — Efficient vs-previous-snapshot `status` (O(latest segment))

## Problem

`yolo status` (default view) classifies the latest segment's changes **vs the
previous snapshot**, but does so in **O(size of journal)** time and memory. In
[`run_status`](../../user/cmd/diff.rs):

```rust
let from_state = Journal::read(&yolofs)?.into_tree_at(start as u64);
```

`into_tree_at(start)` re-reads the journal and **replays every live segment
before `start`** into a full `DirTree`. For the default view `start` is the
second-to-last snapshot, so this rebuilds essentially the whole prior tree on
every `status` / every `yolo -- <cmd>` review. Long sessions become quadratic
(each command pays O(journal)). The same pattern is in `run_after_exec`.

`Changeset::collect` and the notes scan are already O(latest segment); the
*only* O(journal) cost is `from_state`, plus a redundant second `Journal::read`
(`Journal` isn't `Clone`).

## Key enabler (commit b38d897)

The `existed` bit on `Action::Stage` answers exactly the question `from_state`
is rebuilt to answer: *did this path exist at the start of this segment?* The
kernel records it **redirect-resolved** at copy-up time, so:

- create new file → `existed = 0`
- modify a base file / prior-snapshot file → `existed = 1`
- modify a child of a **renamed** dir → `existed = 1` (the redirect resolves to
  the real backing — the case in `test_diff.rs::modify_child_of_renamed_base_dir_is_modified`)

Operationally `existed` means **"existed in the lower at write time" ≈ "existed
in the previous snapshot"** — which *is* the vs-prev baseline. (It is **not**
"vs the immutable base"; see Risks.) So the latest segment + its `existed` bits
are sufficient to classify added/modified/deleted without any prior tree.

## Approach

Replace `from_state` (for the non-verbose `status` path only) with a
`prev_present: HashMap<String, bool>` computed in **one O(segment) pass** over
the live segment records, then classify the net changeset against it.

### `prev_present` — presence at segment start, from the segment alone

Scan the live segments `[start, end)` in record order; for each path, record the
**first** touch (later touches don't change "did it exist at the start"):

| first touch on path        | `prev_present` |
|----------------------------|----------------|
| `Stage { existed }`        | `existed`      |
| `Delete`                   | `true`  (can't delete what isn't there) |
| `Rename` as **src**        | `true`  (can't rename a missing source) |
| `Rename` as **dst**        | `false` (created at dst by the rename)  |
| not in map (moved subtree) | `false` (default) |

### Classification (shared, pure)

```rust
fn change_label(target: &Target, present: bool) -> Option<ChangeKind> {
    match target {
        StagedFile(_) if !present => Some(Added),
        StagedFile(_)             => Some(Modified),
        Tombstone if present      => Some(Deleted),
        Tombstone                 => None,        // no-op delete (create+delete in-segment)
        BasePath(_)               => Some(Renamed),
        Passthrough               => None,
    }
}
```

- **status / `run_after_exec`**: `present = prev_present.get(path).copied().unwrap_or(false)`.
  No content reads, no base `stat`, no prior tree. O(segment).
- **`diff` (verbose)**: UNCHANGED — keeps `from_state`/`from_side`, because the
  unified diff needs the *old content* (and for a renamed-dir child the
  pre-image location needs the redirect chain). Out of scope here.

`print_change` is refactored so the **label** comes from `change_label` in both
paths (status and diff agree by construction); only `diff` additionally resolves
content via `from_side`.

## Correctness — equivalence to today

For every rendered path, `prev_present[path]` must equal
`from_side(from_state, path).exists()` today:

- **Staged path**: `existed` (first stage) ≡ `from_state` presence — proven for
  create (0/absent), base modify (1/base-exists), prior-snapshot modify
  (1/StagedFile), renamed-dir child (1/redirect→base). The kernel already
  resolved the redirect, so we need neither the redirect chain nor a base stat.
- **Tombstone (delete / rename-src)**: present unless created-from-nothing
  earlier in the segment (→ `Stage{existed:0}` first → `prev_present=false` →
  no-op delete, matching `from_state`).
- **Map-miss StagedFile** (staged subtree moved by an in-segment dir rename, no
  later stage): default `false` → "added", matching `from_state` (which has no
  `/newdir` redirect at segment start → base stat misses → "added").
- **Rename source double-render is preserved**: a base-file rename still yields
  both a `(deleted)` line at the vacated source and a `(renamed)` line. This is
  current behavior; this change does **not** alter it (out of scope — note it).

## Edge cases

- Multi-stage of one path in a segment uses **first-touch** existed, not the net
  action's (guards the create-then-delete-then-recreate sequence).
- Re-COW after a snapshot: the modify lands in its own post-snapshot segment, so
  it is the first/only touch there → `existed=1` → "modified" vs prev. Correct.

## Backward compatibility

None needed — the journal is per-session and ephemeral, and the current kernel
always writes the 4th field. The parser **requires** it: an `S` record needs ≥4
fields, and a 3-field record is treated as malformed and skipped, so a truncated
record fails loudly rather than silently classifying as "added". The pre-`existed`
parse-test fixtures were updated to the 4-field form.

## Tests

Host unit (no mount):
- `prev_present` computation across record sequences (create/modify/delete/
  rename/create+delete/recreate).
- `change_label` over all (target × present) combinations.

CLI (VM, `make test-vm`):
- **`status`** version of `modify_child_of_renamed_base_dir_is_modified`
  (the Q4 scenario): rename base dir → snapshot → modify child → "modified".
- re-COW-after-snapshot: create → snapshot → modify → "modified" vs prev.
- Keep `delete_of_staged_file_shows_vs_prev_but_not_full`, `status_renamed`,
  `status_deleted`, `status_modified` green (behavior unchanged).

Journal-level (tests/internals/test_journal_format.rs, closing b38d897 gaps):
- renamed-dir child copy-up records `existed=1`.
- mkdir + symlink record `existed=0`.

## Doc fixes (fold in)

- `parse.rs` format header: `S\0<path>\0<ino>\0<existed>\n` (currently stale).
- `Action::Stage` doc: this change makes the "classify without rebuilding the
  previous tree" claim true; tighten wording to say existed = vs previous
  snapshot (lower-at-write-time), not vs base.

## Scope

`status` + `run_after_exec` only. `diff` stays on `from_state`. No kernel
changes (the `existed` semantics are already correct for vs-prev).
