# 50 — O(segment) `diff` via recorded pre-images (and drop `existed`)

## Goal

Make `yolo diff` classify and render the latest segment vs the previous
snapshot in **O(segment)**, like `status` (plan 49) — without rebuilding the
previous tree (`from_state = into_tree_at(start)`, currently O(journal)).

`diff` needs the *old content*, not just a presence bit, so `existed` alone
can't carry it. Record the **pre-image** (where the old content lives) in the
journal at write time; `diff` reads it directly.

## Key realization (supersedes plan 49's `existed`/`prev_present`)

A pre-image's **presence subsumes the `existed` bit**: a stage that copied
something up has a pre-image (→ "modified"); a fresh create has none (→
"added"). So we **drop**:
- the journal `S`-record `existed` field (added in b38d897), and
- the CLI `Action::Stage.existed` bool + `Changeset.prev_present` map +
  `present_before()` (added in 2c8dc6c).

Both become "is there a pre-image?". Status reads its *presence*; diff reads its
*content*. One source of truth, can't disagree.

## Journal format

Drop `existed`; add a pre-image field to `S` and `D` (NUL-separated):

```
S\0<path>\0<ino>\0<preimage>\n     — Stage   (preimage empty ⇒ fresh create)
D\0<path>\0<preimage>\n            — Delete  (preimage empty ⇒ create+delete in-range)
```

`<preimage>` is the **absolute path of the old content** at write time — the file
COW copied up, or the file a delete hid; empty ⇒ nothing existed (create /
spurious tombstone). It's uniform for both kinds of old content because the base
layer is rooted at `/` (a base file's path is already absolute — `to_base_path`
is essentially identity) and the inode store lives at an absolute location under
the session root:
- base copy-up → `/subdir/deep.txt` (redirect-resolved, so a renamed-dir child
  points at its real backing);
- re-COW of a prior staged inode → `<root>/.yolofs/inodes/<shard>/<ino>`.

Both are just paths the CLI `fs::read`s directly — no tag, no inode-vs-base
dispatch. `R` is unchanged — see Renames.

## Kernel

At COW entry (`yolo_do_cow`) and at delete (`yolo_journal_delete`), the dentry's
`lower_path` *is* the pre-image (COW swaps it only after the copy). Record its
absolute path via `d_path(&lower_path, ...)` — uniform whether the lower is a
base file or a prior staged inode, so no `target` branching and no ino
extraction. Fresh creates (`yolo_create_staged`) write an empty pre-image. If
`d_path` fails (e.g. an unreachable path), write empty and let diff degrade.

(Common-case note: for a base file with no renamed ancestor the pre-image equals
the record's own `<path>` — accepted as redundant rather than special-cased.)

## CLI

- `Action::Stage { path, ino, preimage: Option<String> }`,
  `Action::Delete { path, preimage: Option<String> }` (absolute path; `None` when
  the field is empty).
- `Changeset` carries the **per-change** pre-image, resolved from the **first
  touch** in the range (not the net target — see below):
  ```rust
  pub struct Change { pub path: String, pub target: Target, pub preimage: Option<String> }
  pub struct Changeset { pub changes: Vec<Change>, pub notes: Vec<Note> }
  ```
  This also delivers the plan-49-review cleanup: presence attached to each
  change, no separate map/helper.
- `classify(target, preimage.is_some())` — unchanged rule, shared by status/diff.
- `status`: `render_summary` uses `preimage.is_some()`; ignores content. Still
  O(segment).
- `diff`: old content = `fs::read(preimage)` directly (one path, no inode-vs-base
  dispatch); binary detection via the same null-byte heuristic on those bytes.
  **`from_state` / `into_tree_at` / `from_side` deleted.** Now O(segment) too. A
  missing/unreadable pre-image degrades gracefully (skip the body).

### Why first-touch, not the net change
- **create+delete**: net target is a tombstone, but the first touch (create) has
  no pre-image ⇒ absent ⇒ no-op skip. The delete's own pre-image (the
  session-created inode) must *not* win.
- **multi-segment ranges (`--full`, `--from/--to`)**: a path modified in seg 1
  then seg 3 has net preimage = seg-3's source, but the range-start content is
  seg-1's source. The first touch is the range-start version.
- For the default single-segment view the two coincide (one stage per path), but
  the scan must use first-touch to stay correct for the wider ranges.

## Renames (decided)

A rename shows a **single `(renamed)` entry** (`<src> → <dst>`). The net tree
still tombstones the vacated source, so when building the changeset we **drop a
tombstone whose path is the source of some `BasePath` (rename) target** — it's
the rename's vacate, already shown by the rename line. Doing this at changeset
level keeps status, diff, and the change count consistent. This matches git's
single "renamed" entry, removes the long-standing double-render, and means `R`
needs no pre-image. No existing test regresses (`status_renamed` /
`diff_renamed_file` assert the rename line, which still prints).

## Pre-image lifetime

- Base files are immutable for the session ✓.
- Recorded absolute paths stay valid as long as the session root doesn't move
  (it doesn't mid-session) — kernel and CLI resolve the same locations.
- Prior staged inodes must stay readable at diff time. They persist during a
  normal session (pre-commit). Travel/abort can GC inodes on dead branches —
  confirm a referenced pre-image inode can't be collected while reachable, and
  have diff degrade gracefully (skip the body) if a read fails rather than panic.

## Tests

- Journal-format (internals): `S`/`D` carry the expected absolute pre-image path
  (the base file for a base copy-up, the inode-store path for a re-COW, empty for
  a create); a renamed-dir child records its resolved base path (≠ its overlay
  path).
- CLI diff (VM): modified base file shows the base content; re-COW-after-snapshot
  diffs vs the prior staged inode; renamed-dir child shows a minimal hunk;
  create+delete shows nothing; rename shows one `(renamed)` line (no `(deleted)`).
- Unit: `PreImage` parse (`i`/`b`/empty); first-touch resolution across
  create/modify/delete/rename/create+delete and a multi-segment range.
- Keep the whole existing status/diff/snapshot suite green.

## Migration / scope

- Removes the `existed` field end-to-end (no backward compat — journals are
  per-session; kernel always writes the new form).
- Touches: kmod (journal.c, staging.c, inode.c, yolofs.h), parse.rs, types.rs,
  changeset.rs, cmd/diff.rs, tests. Larger than plan 49 because it spans the
  kernel write path.
- Reshapes `Changeset`/`Action` once (presence + pre-image together).
