# 58 — Journal `pre` hardening: faithful rename folds, unit S records

Follow-up to plan 56 (kernel preimage facts + start/end tree), from a design
review of the journal format and the CLI's `pre` resolution.

## Findings being fixed

1. **Rename fold loses the destination path's first touch.** `apply_rename`
   *replaces* the node at `dst`, so a rename landing on an already-touched
   path drops that path's range-start `start` (e.g. `S /b ino1; R /b←/a`
   leaves `/b.start = s:1`, the in-range intermediate, instead of `b:/b`).
   A later modify then diffs against intermediate content, and a
   delete-then-rename-then-modify chain classifies "modified" as "added".

2. **`apply_rename` is needlessly branchy.** The kernel carries the source's
   backing to the destination verbatim (`inode.c` re-pins `old_dentry` with
   its own target; `d_move` keeps `lower_path`), so for every faithful R
   record **`src_pre` is also the destination's post-rename backing**:
   staged source → `s:<ino>` carried; base/redirect source → `b:<abspath>`
   redirect. The fold can set `dst.end = src_pre` unconditionally instead of
   branching on whether the source node is in-tree and preferring its `end`
   (which equals `src_pre` anyway). The detached node then contributes only
   its children and its first-touch `start`.

3. **S records are best-effort, contradicting the docs.** `yolo_do_cow` and
   `yolo_create_staged` discard `yolo_journal_stage`'s return while D/R
   propagate journal failure. On append failure (ENOMEM, I/O error,
   ENAMETOOLONG past `YOLO_PATH_MAX`) the mount shows a change the artifact
   never records — review and commit silently miss it. docs/staging.md
   already specifies "must succeed as a unit"; make the code match.

4. Smaller items: commit plan ops reuse journal `Action` with fabricated
   `pre` fields; pre tags reuse record-tag letters (`A`, `P`, `I`) in a
   second namespace and don't track the `Target` variant names;
   `Target::BasePath`'s two path namespaces (overlay vs lower absolute)
   coincide only because base ≡ `/` and nothing says so; stale `Passthrough`
   comments from plan 56.

## Changes

### Fold rules (user/journal/tree.rs)

`apply_rename` becomes a flat sequence — no `Some`/`None` match on the
detached source for `end`:

```
detach(src) → (moved_start, children)        // subtree + move-carry start
place src:  start = moved_start || src_pre,  end = Absence
place dst:  start = moved_start || existing-dst.start || dst_pre,
            end   = src_pre,                 children = children
self-redirect collapse unchanged (computed from src_pre vs dst)
```

Start precedence at `dst`, in order: the moved node's `start` (move-carry —
plan 56's deliberate choice: the old side rides with moved content so
`--diff` pairs old/new across the move), then the existing destination
node's `start` (the path's in-range first touch — the fix for finding 1),
then `dst_pre` (op-local clobbered backing, correct when the path is
untouched in-range).

Contract change: the fold now **trusts the kernel record** (`dst.end =
src_pre` verbatim). Unit-test fixtures that fabricate kernel-unfaithful
records (the `rename()` helper passes `src_pre = BasePath(src)` for staged
or redirect sources) are corrected to the values the kernel actually
writes (`StagedFile(ino)` for staged sources; the redirect-resolved base path
for rename chains). Assertions keep their intent.

### Kernel S records become a unit (kmod/staging.c, kmod/inode.c)

Both publish journal-first, so a failed append unwinds trivially (drop the
store-inode ref) and can never produce the bad direction — a mount-visible
change with no journal record:

- `yolo_do_cow`: append the journal record after the content copy but
  *before* the publication block (pin, `staging_gen`/`staging_ino`,
  `lower_path` swap). On failure, `path_put` the store-inode path and return
  the error — the previous mapping stays authoritative; the allocated store
  inode is an orphan like any other (cleared on commit/abort).
- `yolo_create_staged`: order becomes alloc → journal → interpose → pin
  (`dentry_path_raw` works on the still-negative dentry). On journal failure,
  `path_put` the store-inode path and fail the create — nothing published.
  The one post-journal fallible step is interpose's `iget` (rare ENOMEM),
  which leaves a harmless phantom `S` record for an empty orphan inode (the
  safe direction). The interpose-then-undo alternative was rejected: undoing
  a `d_instantiate`'d dentry is a VFS hazard, and a buggy undo would leave
  exactly the silent-divergence this fixes.
- D and R already journal before publication; unchanged.

### Wire format: pre tags (kmod/journal.c, kmod/yolofs.h, user/journal/parse.rs)

No backward compatibility needed; both sides change together. The pre tag
becomes the lowercased first letter of the `Target` variant it parses to —
`a` (Absence), `s:<ino>` (StagedFile), `b:<abspath>` (BasePath). This makes
the tag→variant mapping a single rule and stops the pre namespace from
sharing *case-folded* letters confusingly with record tags by tracking the
type instead of the storage concept (was `A`/`I:`/`P:`). Record tags stay
uppercase `S D R P T A B`. R's field order is unchanged
(`R\0<dst>\0<src>\0<src_pre>\0<dst_pre>\n`).

### Commit plan ops (user/journal/plan.rs, cmd/commit.rs)

`CommitPlan` gets its own op type — `Op::Stage { path, ino }`,
`Op::Rename { dst, src }`, `Op::Delete { path }` — instead of reusing
journal `Action` with fabricated `pre` fields.

### Docs and comments

- staging.md: lowercase pre tags (`a`/`s:`/`b:`), fix the stale example
  journals (they still show untagged raw-path pres), create/COW pseudocode
  journal position.
- `Target::BasePath` doc: record paths are overlay (mount-relative,
  `dentry_path_raw`); `b:` paths are lower-absolute (`d_path`). They are
  interchangeable strings only because base ≡ `/`; the changeset rename
  suppression and commit both rely on that invariant.
- Remove stale `Passthrough` comments (plan.rs, tree.rs).
- Move plan 56 to done/ (verified implemented; only stale comments remained).

## Non-goals (reviewed, deliberately kept)

- **Parse-integrity detection / commit-refusal is not added.** The kernel is
  the sole journal writer with no concurrent access, so interior corruption
  is a thin threat, and a refusing `commit` adds friction for a hypothetical.
  Parse keeps skipping malformed lines as today. (Finding 3 already removes
  the realistic silent-divergence path — a failed S append now fails the op
  instead of leaving an unjournaled change.)
- **R field order is unchanged** (`dst, src, src_pre, dst_pre`). Grouping each
  path with its pre is a readability nicety not worth churning the wire
  format, kernel writer, parser, and every fixture.
- **S/D stay separate tags** — plan 56 chose tags tracking the VFS op kind;
  the fold already unifies them internally, so the split costs one parse arm.
- **Cross-segment staged renames keep classifying as delete+add** — rename
  provenance modeling was explicitly out of plan 56's scope; content stays
  correct.
- **`YOLO_PATH_MAX` stays 256** — widening it ripples through the ask ABI
  structs and kernel stack buffers; with S records now failing loudly, deep
  paths fail the operation instead of silently diverging. Follow-up if deep
  trees matter.
- **Gen widths stay u16 kernel-side** — already a deliberate, guarded cap
  (`>= U16_MAX` checks in ioctl.c before snapshot/travel).

## Tests

- tree unit (failing first): rename over a touched destination keeps the
  path's first-touch `start`; rename over a tombstoned-then path keeps its
  `start`; move-carry still wins when the moved node has a `start`
  (regression pin); `dst.end == src_pre` verbatim for staged sources.
- parse unit: `a`/`s:<ino>`/`b:<path>` round-trip to the right `Target`;
  malformed `s:` value still skips the record (unchanged behavior).
- internals VM: a base-file COW carries `b:<path>`, a re-stage carries
  `s:<ino>`, a fresh create carries `a`; a staged-source rename carries
  `s:<ino>` src_pre (the existing format tests, re-tagged).
- internals VM (failing first): a deep-path mutation (> `YOLO_PATH_MAX`)
  fails the operation instead of succeeding with no journal record.
- Existing suites stay green modulo fixture corrections listed above.
