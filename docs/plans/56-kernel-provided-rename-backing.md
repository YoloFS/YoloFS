# 56 - Kernel preimage facts + start/end tree

## Goal

Make `yolo review` a fast, correct net comparison of the selected range:

```
range start state -> range end state
```

without rebuilding the previous tree, statting base, or joining two separate
path-keyed state maps.

This plan merges the two pieces that only make sense together:

1. The kernel records complete pre-op preimage facts for renames.
2. Userspace folds the selected range into one sparse `DirTree` whose nodes
   carry both `start` and `end` targets.

## Problem

Today S and D records carry the kernel-resolved preimage path of displaced
content, but R records carry only overlay paths. The CLI re-derives rename
backings with `resolve_base_path` during tree replay. Separately,
`Changeset::collect` builds a `preimage` map keyed by record-time overlay paths,
then joins it against the range-end tree by string equality.

Those two shortcuts break as soon as renames shift paths:

| Session | Today | Required |
|---|---|---|
| `vi d/f; mv d e` | `/e/f` misses `/d/f`'s preimage, so it shows added and `--diff` loses old content | `/e/f` carries the old side from `/d/f` and diffs correctly |
| `rm d/f; mv d e` | net tombstone is at `/e/f`, preimage entry is at `/d/f`, so the delete is hidden | `/e/f` renders as deleted |
| `mv a b; rm b` | no record seeds source key `/a`, so review shows nothing while commit deletes `/a` | `/a` renders as deleted |
| rename over existing `b`, then rewrite `b` | destination clobber is invisible, so review can show added | old `b` is available as the diff/delete old side |

The required property is correctness of the net comparison. Fancy rename
presentation such as `renamed (modified)` is not a goal here.

## Layer Boundary

The kernel should emit operation-local facts. It should not emit review
"before" state or classifications.

Reason: review ranges are selected later by userspace:

```
yolo review        # latest segment vs previous snapshot
yolo review all    # base -> tip
yolo review 1..3   # snapshot 1 -> snapshot 3
yolo review --each # many adjacent ranges
```

The same journal record can have different range-start meaning in different
queries. The kernel only knows the current VFS operation; userspace knows the
range.

## Journal Preimage Fields

One rule, journal-wide: a `*pre` field records the `Target` that backed that
overlay name immediately before the operation — the operation-local **pre**-op
state. It is *not* range-scoped (userspace picks the range) and *not* resolved
down to the immutable base:

```
A              -> Target::Absence
I:<ino>        -> Target::StagedFile(ino)
P:<abs-path>   -> Target::BasePath(abs-path)
```

The kernel formats this from pre-op dentry state. Base/redirect content uses
the redirect-resolved `lower_path` (`P:`); already-staged content uses the
staged inode id (`I:<ino>`) — the *exact* pre-op backing, deliberately not the
original base the inode was COW'd from. First-touch-in-range then yields the
true range-start version for `--diff`: a file staged in a prior snapshot diffs
against that snapshot, not the base. If the kernel cannot resolve a path it
degrades to `A`, matching the existing "no old side available" review behavior.

Fields without the suffix are overlay names. The field is named `pre` (not
`preimage`, and *not* `lower`: "lower" is an overlayfs base-layer term, but this
value can point at a staged inode — upper/staged content, the opposite of a
lower layer). `pre`/`post` is the operation-local axis; the userspace tree's
`start`/`end` is the separate range-scoped axis, so the names stay distinct.

S and D stay separate tags and keep their wire *shape* (field count); only the
`pre` field's *content* changes, from a raw path / empty to the tagged `Target`
form. The post-target is implicit in the tag — `S` ⇒ `StagedFile(<ino>)` (via
its bare `<ino>` field), `D` ⇒ `Absence`:

```
S\0<path>\0<ino>\0<pre>\n
D\0<path>\0<pre>\n
```

Keeping `S`/`D` split (rather than one `E` record) keeps each tag aligned with
the VFS op that produced it, keeps the staged `<ino>` a first-class bare integer
instead of a re-parsed `I:<ino>` string, and mirrors `R` staying its own tag:
tag count tracks operation kind (create/COW, delete, rename).

R carries move semantics and two pre fields:

```
R\0<dst>\0<src>\0<src_pre>\0<dst_pre>\n
```

The R fields form a source/destination matrix:

| | source entry | destination entry |
|---|---|---|
| overlay name | `src` | `dst` |
| pre (pre-op backing) | `src_pre` | `dst_pre` |

No backward compatibility is needed; journals are per-session and the kernel
always writes the new form.

### Parse-time resolution

The parser resolves every tagged `pre` field into a `Target` (below) once.
No raw path travels around ambiguously, and userspace does not sniff
`.yolofs/inodes/` layout:

```
A       -> Target::Absence
I:<n>   -> Target::StagedFile(n)
P:<p>   -> Target::BasePath(p)
```

Malformed `I:` values are skipped with the malformed record. Unknown tags are
malformed records, not compatibility fallbacks.

## Kernel Changes

`yolo_journal_rename` captures both preimages before any dentry state changes:

```c
src_pre = yolo_preimage_target(old_dentry, src_buf2, sizeof(src_buf2));
dst_pre = d_is_positive(new_dentry)
        ? yolo_preimage_target(new_dentry, dst_buf2, sizeof(dst_buf2)) : "A";
journal_write(sbi, 'R',
              (const char *[]){ dst_path, src_path, src_pre, dst_pre, NULL });
```

Notes:

- The caller already journals before `d_move`.
- A negative `new_dentry` (fresh name or pinned tombstone) has no backing, so
  `dst_pre` is `A`.
- `yolo_inode_info` gains a `staging_ino` field so `I:<ino>` does not require
  parsing the inode-store path. It is set wherever a dentry is pinned to
  `YOLO_TARGET_INODE` with a known id — `yolo_create`/`mkdir`/`symlink`
  (`inode.c`), `yolo_do_cow` (`staging.c`), and travel/mount restore
  (`ioctl.c`, alongside the existing `staging_gen` set) — so a rename of a
  staged file resolves `I:<ino>` even after remount.
- The same `yolo_preimage_target` helper feeds S and D, not just R. `yolo_create`
  passes `A` (fresh create, nothing existed); `yolo_do_cow` computes the pre from
  the dentry *before* the `lower_path` swap; `yolo_journal_delete` computes it
  from the still-positive dentry before `d_drop`. The old `yolo_lower_abspath`
  calls in those writers are replaced. So all of S/D/R emit the tagged form.
- `journal_write`'s buffer grows from `3 * YOLO_PATH_MAX + 64` to
  `4 * YOLO_PATH_MAX + 64` (R now carries four path-ish fields).

## Userspace Data Model

One tree carries both ends of the comparison — `start` and `end` ride on the
same node. Do **not** build a separate range-start tree and diff two trees by
path: a rename shifts paths, so a path-keyed diff mis-renders `vi d/f; mv d e`
as a delete of `/d/f` plus an add of `/e/f` and loses the old-content linkage.
Keeping `start` on the node that the rename moves is exactly what carries the
old side across the move. This is the property the whole plan exists to get.

Keep the existing `Target` enum (the name matches the kernel's `target`), with
its existing variants — only `Passthrough` is removed:

```rust
enum Target {
    StagedFile(u32),  // content is a staged inode in .yolofs/inodes/<shard>/<ino>
    BasePath(String), // content lives at this base-filesystem path
    Absence,           // no content here
}

struct DirNode {
    start: Option<Target>, // None = no first-touch metadata yet (scaffold)
    end: Option<Target>,   // None = scaffold directory only
    children: DirTree,
}
```

The same `Target` is the one currency for all three roles — `pre`, `start`,
and `end`. `BasePath` holds a real base-filesystem path. The parsed records
carry targets directly:

```rust
Stage  { path, ino: u32, pre: Target }   // post is implicitly StagedFile(ino)
Delete { path, pre: Target }             // post is implicitly Absence
Rename { dst, src, src_pre: Target, dst_pre: Target }
```

`Passthrough` is gone: a scaffold dir (one that only provides a path to deeper
nodes) is `end = None`. `None` is not a filesystem state — it is fold/scaffold
bookkeeping. A real touched entry has `Some(start)` and `Some(end)` after
folding.

The field supplies the time axis:

- `start = Some(Target::BasePath(p))`: old content for review lives at base `p`.
- `end = Some(Target::BasePath(p))`: final entry redirects to base `p`.
- `start = Some(Target::Absence)`: first touch found nothing at range start.
- `end = Some(Target::Absence)`: final tree contains an absence/tombstone.

Do not add separate `Baseline`, `BeforeTarget`, `EntryState`, or `ContentRef`
enums — `Target` covers pre/start/end.

## Fold Helper

Because pres arrive already resolved to `Target`, the fold needs no
string-to-target conversion (no `start_from_lower` / `end_from_rename_lower`):
`start` is the parsed `pre` directly, and a rename destination's `end` is the
source's parsed `src_pre` directly. The only helper is first-touch
assignment:

```rust
fn assign_start_once(node: &mut DirNode, target: Target) {
    if node.start.is_none() {
        node.start = Some(target);
    }
}
```

Review can read the old side off either `Target::StagedFile` (via the session
inode path) or `Target::BasePath` (the disk path), so leaving a staged `pre`
as `StagedFile` on the `start` side is fine — there is no need to keep it as a
path.

## Fold Rules

First touch assigns `start`; later operations update only `end`.

### Stage

```
S path ino pre
```

- Walk/create `path`.
- `assign_start_once(node, pre)`.
- Set `node.end = Some(Target::StagedFile(ino))`.
- Preserve children.

### Delete

```
D path pre
```

- Walk/create `path`.
- `assign_start_once(node, pre)`.
- Set `node.end = Some(Target::Absence)`.
- Preserve children. Commit already treats an absent directory as deleting the
  subtree.

### Rename

```
R dst src src_pre dst_pre
```

Rename is the only operation where "state at a path" and "state carried by
moved content" diverge. Keep that logic local to `apply_rename`.

1. Detach the source node if it exists.
2. If the source node exists:
   - Move it to `dst`.
   - If its `end` is `None` (a scaffold directory being explicitly renamed), set
     `end = Some(src_pre)`.
   - Keep its existing `start` when present; this carries modified children
     through a directory rename (`vi d/f; mv d e`). If `start` is still `None`
     (a scaffold directory), seed it from `dst_pre`.
3. If the source node does not exist:
   - Create a destination node with `end = Some(src_pre)`.
   - Seed its `start` from `dst_pre`. This records the destination position
     that was clobbered, if any.
4. Always create/replace the source node with `end = Some(Target::Absence)`.
   - Its `start` is the detached node's `start` if present, else `src_pre`.
   - This is the critical rule for `mv a b; rm b`: even if the destination later
     disappears, `/a` still has old content and `end = Absence`.
5. A net self-redirect (`end = Some(Target::BasePath(p))` where `p` is the node's
   own path) with no children is a no-op — remove the node so commit doesn't
   emit a self-move (`into_plan` would otherwise turn it into `Rename { dst: p,
   src: p }`). This can't hide a real delete: a self-redirect only arises from a
   roundtrip (`a→…→a`), where content returns to its origin; any genuine deletes
   in the chain live at other paths and fold independently.

This plan deliberately does not introduce `renamed_from` or a full rename
provenance model. A plain rename can still render as a rename when the final
`end` is `Target::BasePath(src)`. Modified-after-rename may render as a delete
plus an add/modify. That is acceptable as long as review does not miss the real
change and `--diff` has the correct old content.

## Review Projection

`Changeset::collect` no longer derives state.

It should:

1. Make one O(segment) pass to collect/dedupe A/B notes only.
2. Build the `DirTree` for the selected live range.
3. Flatten tree nodes into review changes:

```rust
struct Change {
    path: String,
    start: Option<Target>,
    end: Option<Target>,
}
```

4. Suppress vacated rename sources, then classify.

Per-node classification is a small pure function:

| start | end | Review |
|---|---|---|
| `None` | `None` | skip scaffold |
| `Absence` | `Absence` | skip no-op |
| `Absence` | `StagedFile/BasePath` | added, or renamed if rendered from `BasePath` |
| `BasePath/StagedFile` | `Absence` | deleted |
| `BasePath/StagedFile` | `StagedFile` | modified; diff old side from `start`, new side from the staged inode |
| `BasePath/StagedFile` | `BasePath` | renamed/redirect metadata |

Classifying nodes independently would double-report a plain rename: `mv a b`
leaves `/a` (`start = BasePath(/a)`, `end = Absence` → *deleted*) **and** `/b`
(`start = Absence`, `end = BasePath(/a)` → *renamed*), so it would render both
`a (deleted)` and `a → b (renamed)`. One cross-node pass removes the redundant
source — the direct generalization of today's `rename_sources` drop, keyed on
the base path instead of the overlay source:

```
moved_to = { L | some surviving node has end = Some(Target::BasePath(L)) }
skip a node when end = Some(Target::Absence)
            and start = Some(Target::BasePath(L)) with L ∈ moved_to
```

This is correct precisely because it keys on a *surviving* destination: in
`mv a b; rm b` the destination is later deleted, so no `end = BasePath` survives,
`moved_to` is empty, and `/a`'s delete is **not** suppressed — review still
shows `a (deleted)`. It also suppresses the vacated `/d` in `vi d/f; mv d e`
(`/e` carries `end = BasePath(/d)`), leaving `d → e (renamed)` + `e/f (modified)`.
There is no over-suppression: a single base path `L` backs one content origin,
so it can't be both a live rename destination and an unrelated deletion's
`start`.

There is no path filter (`yolo review -- <path>` is dropped — see below), so
the positional spec is unambiguously a range and step 4 is the only filtering.

Defensively treat missing `start` for a rendered non-scaffold node as
`Absence`, but tests should cover the fold so this does not happen for real
touched entries.

The old-side reader should accept any present `start` target:

- `Target::BasePath(path)` -> read that disk path.
- `Target::StagedFile(ino)` -> read the session inode path.

## What Gets Removed

- CLI backing re-derivation for R records (`resolve_base_path` and
  `detach_resolved`).
- The `pre` side map and path-string join in `Changeset::collect`.
- `Target::Passthrough` as a variant; scaffolds become `end = None`. The three
  traversal filters that skip `Passthrough` today — `DirTree::len`,
  `DirTree::get`, and `visit_targets` — flip to skip nodes with `end.is_none()`.
- The `start_from_lower` / `end_from_rename_lower` conversion helpers are never
  introduced — the parser resolves `pre` fields to `Target`, so the fold uses
  them directly. No fold-time `is_session_inode_store_path` / `parse_ino`, and no
  inode-store prefix plumbed into the tree builder.
- The `yolo review -- <path>` filter and `Target::matches_path` (with its
  "match src or dst" rename special case). The new model would make the filter
  ambiguous, and it earns little; `yolo journal`'s own `-- <path>` filter
  (`action_matches_path`, separate code) is unaffected. Drop its `tests/cli`
  coverage and the `docs/cli.md` mention.
- Any requirement to model advanced rename provenance in review.

End state of the CLI pipeline:

```
parse  -> raw journal records with pre fields resolved to Target
fold   -> one sparse DirTree with start/end targets
views  -> commit, travel, review projections
```

Commit and travel read `end` only. Review reads `start + end`.

## Tests

- Parse unit: 4-field S round-trips `<ino>` and the tagged `pre`; 3-field D
  round-trips the tagged `pre`; short S/D records are skipped.
- Parse unit: 5-field R round-trips `src_pre` / `dst_pre`; short R
  records are skipped.
- Parse unit: `I:<ino>` parses to `Target::StagedFile(ino)`, `P:<path>` to
  `Target::BasePath(path)`, and `A` to `Target::Absence`; malformed tags or
  `I:` values skip the record.
- Tree unit: `Target` mapping and scaffold behavior (`end = None`).
- Tree unit: rename uses the record's `src_pre`/`dst_pre` rather than
  tree-walk backing resolution; `resolve_base_path` and `detach_resolved` are
  gone.
- Tree/review unit: first-touch precedence (`create + delete + recreate` stays
  added; `touch a; rm a` nets to no rendered change).
- Tree/review unit: `vi d/f; mv d e` carries the old side to `/e/f` and diffs
  from `/d/f`.
- Tree/review unit: `rm d/f; mv d e` renders deleted `/e/f`.
- Tree/review unit: `mv a b; rm b` renders deleted `/a` (no surviving
  `end = BasePath`, so the source is not suppressed).
- Tree/review unit: plain `mv a b` renders a single `a → b (renamed)` line; the
  vacated `/a` is suppressed, not shown as a separate delete.
- Tree/review unit: rename-over-existing seeds the destination from
  `dst_pre`.
- Tree unit: `A` `src_pre` degrades the rename destination to
  `Target::Absence` — no invented overlay path.
- Internals VM: S record for a base-file COW carries `P:<path>` as `pre`; a
  re-stage of an already-staged file carries `I:<ino>` as `pre`; a fresh create
  carries `A`. D record for a base file carries `P:<path>`.
- Internals VM: R record for base-file rename carries the base path as
  `P:<path>` `src_pre` and `A` `dst_pre`; rename of a child under a
  renamed dir carries the redirect-resolved `P:<path>` `src_pre`; rename
  onto an existing base file carries that file as `P:<path>` `dst_pre`;
  rename of a staged file carries `I:<ino>` as `src_pre`.
- CLI/VM: `yolo review` and `--diff` for renamed-dir-child and
  rename-then-delete sessions.
- Existing status/diff/snapshot suites stay green. Plain renames still render as
  a single line (source suppressed); only modified-after-rename may render as a
  delete plus an add/modify — intentionally less rename-polished but more
  correct.

## Docs to Update First

- `docs/staging.md` journal format: S and D stay separate tags; their `pre`
  field becomes the tagged `Target` form. R's wire line and table row gain
  tagged `src_pre` and `dst_pre`. Extend the per-field prose to document `A`,
  `I:<ino>`, and `P:<path>`. (Revert the in-progress `E`-unification edit.)
- `docs/staging.md` rename handling: the pseudocode currently journals *after*
  `yolo_dentry_pin(old_dentry, …)`; move the `journal(R, …)` call above the pin
  block and add the two target-preimage captures, so both preimages are taken
  before any dentry state change (matching the real `inode.c` order). The tree
  builder consumes the record preimages.
- `docs/staging.md` review section: rewrite the `Changeset::collect` /
  classification / `--diff` steps — describe the sparse start/end tree (and the
  vacated-source suppression) and remove the preimage side-map + first-touch
  wording.
- `docs/staging.md` travel wire-format note: the line "Passthrough dirs are
  encoded as PATH with `path_len=0`" should say "scaffold dirs (`end = None`)".
  The userspace `Target` variant names are otherwise unchanged; only
  `Passthrough` is removed. (The commit op walk needs no change — `BasePath` /
  `StagedFile` / `Absence` all survive.)
- Header comments duplicating the journal format: `kmod/journal.c`,
  `user/journal/parse.rs`.

## Touches

kmod (`journal.c`), user (`journal/parse.rs`, `journal/types.rs`,
`journal/tree.rs`, `journal/plan.rs`, `changeset.rs`, `cmd/review.rs`,
`cmd/journal.rs`, travel serialization helpers), tests, docs.
