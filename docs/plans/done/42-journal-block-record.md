## Why

The journal currently records only state mutations (S/D/R) and metas (M/J).
It has no trace of *blocked* accesses. When an agent fails ("permission
denied") the user has no after-the-fact way to see which paths yolofs
rules blocked, in what order, relative to which checkpoint. This hurts
debugging ("why did make fail?"), rule refinement ("agent keeps hitting
deny on `/usr/include` — promote to `ro`"), and audit.

A separate file (e.g. `.yolofs/blocked`) loses ordering and timing
relative to S/D/R/M/J. Putting the records into the journal preserves the
timeline naturally and reuses `yolo audit` (including `--path` filtering).

## Goals

- Add a new journal record type `B\0<path>\n` written by the kernel when
  a rule blocks an access with `-EACCES`.
- Surface B records in `yolo audit` (reachable / dimmed when unreachable)
  with `--path` filter support.
- Keep B observational: no effect on staging state, the dir tree, commit,
  abort, restore, status, or diff.
- Keep the kernel record format minimal — only the path. No op, perm,
  pid, or comm fields in this iteration.

## Non-goals

- No in-kernel dedup / rate limiting (may revisit if spam becomes an issue).
- No logging for `HIDDEN` (`-ENOENT`) — only `-EACCES` blocks. Hidden
  paths often deny pre-lookup and need different plumbing.
- No new CLI subcommand or `--blocked` filter for `yolo audit` — blocked
  records show in the normal stream alongside actions and metas.
- No userspace-side persistence beyond the journal (no separate audit file).
- No other `Note` variants in this plan. `Note::AskOutcome` (one-shot ask
  decisions, including timeout/default-deny) is a natural follow-up and
  will reuse the same infrastructure additively. Designing `Note` as the
  category of *permission-layer observations* (not a one-off "Block" hook)
  makes the next variant a small additive change.

## Format

```
B\0<path>\n
```

`<path>` is the overlay path the user tried to access (file for file
opens; child target for parent-write-denied mutates such as create or
unlink). `dentry_path_raw()` produces it the same way S/D/R do.

B records are observational, not mutations:

- Do **not** set `sbi->dirty` (would cause spurious auto-checkpoints after
  read-only commands that hit deny rules).
- Do **not** take `staging_sem` (no COW serialization needed).
- Always live alongside S/D/R within a segment for reachability — if a
  restore renders the segment unreachable, B is dimmed too. This matches
  existing audit semantics; no new special case.

## Plan

### Kernel

1. Add `yolo_journal_block(sbi, dentry)` in `kmod/journal.c`, mirroring
   `yolo_journal_delete`. Update the file-header comment block to include
   the B format.
2. In `journal_write()`, extend the dirty exclusion to `'B'`:
   `tag != 'M' && tag != 'J' && tag != 'B'`.
3. Declare the prototype in `kmod/yolofs.h`.
4. Emit on `-EACCES` at three sites, always using the **target** dentry
   (the user-intended path), not the parent whose perm caused the block.
   All three sites are yolofs-rule-only by construction: each `-EACCES`
   originates from a yolofs perm decision, never from lower-FS DAC.
   - `kmod/inode.c` (`yolo_permission`) on the `-EACCES` branch (explicit
     `deny`/`ro+write` rule on a regular file open — the common hot path,
     reached *before* `yolo_open`). Use `d_find_alias(inode)` to recover
     a dentry; cheap because it only fires on denial and regular files
     have one alias.
   - `kmod/file.c` (`yolo_open`) after `yolo_check_dentry_perm(...)` returns
     `-EACCES` — catches `ask`-resolved-deny (where `yolo_permission`
     returned 0 for `ASK`) and rule-change races.
   - `kmod/inode.c` (`yolo_check_mutate_perm`) after parent perm denies the
     mutate — child dentry passed into the function. Lower-FS DAC denials
     on the parent never reach this path; the yolofs mutate check is
     separate from the VFS `inode_permission` on the parent dir.

### Userspace

Add a `Note` variant — distinct from `Action` and `Meta` — so the type
system enforces "this is observational" everywhere.

1. `user/journal/types.rs`:
   - `pub enum Note { Block { path: String } }`
   - Extend the existing `Record` enum with a third variant
     `Record::Note(Note)`.
   - Change `Segment.records: Vec<Action>` → `Vec<Record>`. The invariant
     that `Record::Meta` is never pushed into a segment is already
     maintained by `Journal::new` (metas split segments); notes follow the
     same construction.
2. `user/journal/parse.rs`: parse `b"B"` (≥ 2 fields) into
   `Record::Note(Note::Block { path })`. Update the file-header comment
   block.
3. `user/journal/core.rs` (`Journal::new`): on `Record::Note(_)` push it
   onto `current_records` (same path as Action today). Metas still split
   segments.
4. `user/journal/tree.rs` (`DirTree::build`): iterate `seg.records` and
   match: `Record::Action(a) => self.apply(a)`, `Record::Note(_) => {}`,
   `Record::Meta(_) => {}` (won't be reached given the construction
   invariant).
5. `user/cmd/audit.rs`: iterate `seg.records`. Add a formatter for
   `Note::Block` (e.g. `"blocked"` in yellow + path). Extend
   `action_matches_path` (or add a sibling helper) to filter notes by path.
6. Other consumers (`cmd/diff.rs`, `cmd/status.rs`, `cmd/commit.rs`): the
   compiler will surface any non-exhaustive matches on the new `Record`
   shape inside segments. Each ignores notes (filter to `Record::Action(_)`
   before processing).

### Docs

1. `docs/staging.md` — update "Journal Format" code block + table to add
   the B row. Add a short paragraph noting B is observational (no dirty,
   no tree contribution, no commit/abort effect; subject to reachability
   like S/D/R). Note current scope: `-EACCES` only, not `HIDDEN`/`-ENOENT`.
2. `docs/permissions.md` — under "What is gated", mention that blocks are
   recorded in the journal and surfaced via `yolo audit`.

### Tests

1. `user/journal/parse.rs`: parse one well-formed `B`, malformed `B`
   skipped (only the tag, no path).
2. `user/journal/tree.rs`: a sequence with `Note::Block` interleaved with
   S/D/R yields the same `DirTree` as without it.
3. `user/cmd/audit.rs`: `format_note(&Note::Block { path: "/etc/x" })`
   contains `"blocked"` and `"/etc/x"`. `--path` filter applies to notes.
4. `tests/perm/`: set a `deny` rule on `/etc`, attempt `cat /etc/passwd`
   inside `yolo exec`, then assert `.yolofs/journal` contains
   `B\0/etc/passwd\n`.
5. `tests/perm/`: set a `deny` rule on `/etc`, attempt `touch /etc/foo`,
   assert the journal contains `B\0/etc/foo\n` (target child, not parent).
6. `tests/perm/`: confirm `dirty` semantics — read-only command that
   triggers B-only writes does **not** cause an auto-checkpoint when
   `yolo exec` finishes with `YOLO_MARK_IF_CHANGED`.

### Verification

`make test-vm`.
