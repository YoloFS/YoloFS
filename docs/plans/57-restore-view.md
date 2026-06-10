# 57 — `RESTORE` ioctl: rebuild the staged view on mount

Prerequisite for plan 54's orthogonal lifecycle: an existing `.yolofs/`
artifact must rebuild its live view whenever it is mounted.

## Problem

Staged state lives only in kernel memory: pinned dcache entries built
op-by-op during a live session. The journal is opened write-only
(`journal.c` `yolo_journal_open`, `O_WRONLY | O_APPEND`) and never read by
the kernel; `yolo_lookup` falls through to the base only; `next_ino` and
`staging.gen` are hard-reset to 0 on every mount (`super.c`). So mounting
over a surviving `.yolofs/`:

- shows the **base**, silently dropping the staged view, and
- corrupts the artifact on the next write: `next_ino` restarts at 1,
  colliding with surviving `.yolofs/inodes/` entries (`vfs_create` →
  `EEXIST`) and appending journal records whose ino keys duplicate the old
  session's; marker generation ids likewise restart and collide.

Plan 54 assumed replay-on-mount existed. It doesn't — but `travel` already
built ~90% of it: userspace computes a view from the journal
(`Journal::into_tree[_at]` → `DirTree::serialize`) and the kernel
materializes it (`yolo_travel_inject`: staged-ino links, tombstones for
deletes, base-path redirects for renames). What `TRAVEL` adds on top is
travel *bookkeeping*: gen increment, `T` record, `dirty = false`. And
`RESET` is the degenerate case: set the view to the bare base, gen 0.

This plan factors out "set the view from a tree" as one ioctl, `RESTORE`,
uses it for mount-time replay, and collapses `RESET` into it.

## Design

### Name: `RESTORE`, subsuming `RESET`

The shared mechanism is: quiesce the old overlay, then build a new one from
a serialized tree. `RESTORE` is its name, and the empty tree is its
baseline case — *restore the view to the base*. The `git restore` analogy
is exact: `git restore <path>` from a source brings back saved content;
`git restore .` discards to the baseline. Both are "restore"; the target is
what differs. So one ioctl covers both intents:

- **rebuild from the journal** — mount-time replay (a journal-derived
  tree).
- **clear to the base** — `abort`, post-`commit` (the empty tree).

`RESTORE` over the alternatives: it sets only the kernel's in-memory
**view**, never the durable artifact (journal + inode store, which stay
userspace-owned) — so `SET_STATE` would overclaim — and "restore" names the
rebuild-from-the-saved-artifact direction that `SET_VIEW`/`PUT_VIEW`
understate. `RESET` is deleted; it was always the empty-tree case of this.

### Arguments: each value lives with the layer that can verify it

`RESTORE`'s header is `{ tree_ptr, tree_len, gen, dirty, max_ino }` — the
tree plus journal/store facts the kernel cannot read cheaply:

- **tree** — validated at inject time, as for travel today: structural
  bounds, ino existence (`yolo_inode_path` fails on a missing store file),
  base-path resolution for redirects. An invalid tree fails the ioctl; it
  cannot dangle references the kernel didn't check.
- **`max_ino`** — argument. Userspace derives it as the maximum inode id in
  every S record across the full journal, including dead segments. The kernel
  sets `next_ino = max(current, max_ino)`, so
  reset with `max_ino = 0` never moves the live counter backwards. A value
  that is too low causes the next colliding allocation to fail with
  `EEXIST`; it does not silently overwrite an existing inode.
- **`gen`** — argument. The current generation is a journal fact (the last
  marker's id); the kernel can't read the journal, and a wrong value affects
  both userspace marker numbering and future COW behavior — the same trust
  already extended to `TRAVEL`'s `target_gen`. RESTORE conservatively stamps
  injected staged inodes with `staging_gen = max(gen - 1, 0)`, forcing their
  first post-remount write through COW so snapshot-owned inode content cannot
  be mutated in place. TRAVEL continues stamping its injected view with its
  new generation.
- **`dirty`** — argument. "Live S/D/R since the last P/T marker" is also a
  journal fact, and it drives `SNAPSHOT_IF_CHANGED`. Deriving it kernel-side
  as "tree non-empty" would auto-snapshot a restored session whose staged
  work was already snapshotted before teardown — a spurious duplicate
  marker. One byte buys exact resume semantics: the first `yolo run` after
  a restore snapshots (or skips) exactly as if the session had never been
  torn down.

The crafted-invalid-state surface reduces to gen/dirty, whose blast radius
is the caller's own journal bookkeeping — no worse than `TRAVEL` today. And
although `RESTORE` is now shared by replay and abort/commit, only **one**
caller (mount replay) ever passes a non-trivial header: the abort/commit
path passes the constant `(empty tree, gen = 0, dirty = false)`, nothing to
craft.

### Kernel changes

- Extract `yolo_set_view_locked(sb, sbi, tree, gen, inode_gen)` = the `fd_count`
  check + `yolo_staging_quiesce` + generation assignment +
  `yolo_travel_inject` body, shared by `RESTORE` and `TRAVEL`. Its caller
  holds `staging.sem` for write. `TRAVEL`'s
  behavior is unchanged (lock, `fd_count` check, gen++, set view, journal
  `T`, `dirty = false`) and stays a single atomic bundle — the `T` record
  must be appended under `staging.sem`, ordered against concurrent S/D/R
  appends, so travel cannot be decomposed into userspace-visible halves.
- New `YOLO_IOC_RESTORE`: validate `gen <= U16_MAX`, lock, call
  `yolo_set_view_locked`, then set `dirty` and advance `next_ino` to at
  least `max_ino`. No journal record, no gen increment.
- **Delete `YOLO_IOC_RESET`**: the live-view clear step becomes
  `RESTORE(empty tree, gen = 0, dirty = false)`, exactly RESET's semantics.
  Commit/abort issue it only when a live view is mounted; unmounted artifact
  changes need no ioctl.
- **Gating**: add `RESTORE` to the refused-from-inside list
  (`yolo_caller_inside`). A `TARGET_PATH` redirect resolves an arbitrary
  caller-supplied path with `kern_path()` — from inside the mount that maps
  any allowed name onto any host file, defeating permission gating.
  **Add `TRAVEL` to the same list**: it has the identical redirect
  mechanism and is currently callable from inside (the dispatch comment
  calling TRAVEL "fine from inside" is wrong on this point). All legitimate
  callers — mount replay, commit, abort, the `travel` CLI — run host-side;
  verify `ioctl::open`'s `/`-fallback path is not relied on by them.
  Note this tightens the clear-to-base path: the old `RESET` was
  inside-callable, and folding it into a refused-from-inside `RESTORE`
  refuses clear-from-inside too. That's safe — `abort`/`commit` are
  host-side, and nothing inside the mount should reset staging — but it is
  a deliberate behavior change, not just a refactor. (`snapshot`/`RESOLVE`
  stay inside-callable as today.)
- Failure semantics, documented: a non-empty inject can fail mid-tree
  (missing store ino; vanished base path under a rename redirect). As with
  travel today, the injected prefix stays pinned — the view is undefined
  and the caller must resolve it (replay's policy below; travel's existing
  retry-or-abort note).

### Userspace changes

- `ioctl.rs`: drop `reset()`; add
  `restore(fd, gen, dirty, max_ino, tree_buf)` with
  the new header struct. Update the abort/commit live-view clear helper to
  call `restore(empty, 0, false, 0)` before clearing the artifact when a live
  view exists; skip the ioctl when no live view exists.
- `Journal`: cache restore facts while segmenting records: latest marker
  generation, dirty tail state (any S/D/R record after the last P/T marker),
  maximum S-record inode id across the full journal including dead segments,
  and whether any live segment has staged changes.
- `mount::mount()` — the restore step, after `do_mount` succeeds and before
  reporting: when an artifact existed before mount, always build
  `into_tree()`, serialize, and call
  `restore(tree, latest_gen, dirty, max_ino)`, even when the live tree is
  empty (marker generation still needs restoring). Only the announce line is
  conditional on `has_staged_changes`. On failure, tear down only the live
  view while preserving `.yolofs/`, then bail with
  `staged changes could not be restored — `yolo review`/`commit`/`abort`
  work without mounting`. The artifact is untouched by a failed restore;
  only the (now unmounted) view was.
- Base-drift caveat to document in `docs/staging.md`: while unmounted, a
  rename redirect's base source can vanish; that surfaces as the
  restore-failure path above. (Plan 56's kernel-provided rename backing
  touches exactly this mechanism — coordinate if it lands first.)

### Docs

- `docs/architecture.md`: the view-as-projection model (mount = live, gated
  view of the journal + store artifact, rebuilt on mount via `RESTORE`);
  the ioctl contract and the ownership principle above; `next_ino`
  userspace-provided `max_ino`; `RESET` removal; the gating-list change and why
  redirects are gating-defeating.
- `docs/staging.md`: staging durability across unmount/reboot now holds for
  the *view* too, not just the artifact; base-drift caveat.

## Steps

1. Docs first: `architecture.md`, `permissions.md`, `staging.md` per above.
2. Kernel: extract `yolo_set_view`; add `RESTORE`; delete `RESET`; refactor
   `TRAVEL` onto the helper; gate `RESTORE` + `TRAVEL` inside-refused.
3. Userspace: `restore` ioctl wrapper replacing `reset`;
   cached `Journal` restore fields; restore step in
   `mount::mount()` with announce + artifact-preserving unmount-on-failure.
4. Tests:
   - `tests/internals/`: stage → tear down the view leaving `.yolofs/`
     intact → mount → staged content visible through the mount; deleted
     files stay hidden (tombstones); renamed files appear at dst.
   - ino continuity: staging after a restore allocates fresh inos (no
     `EEXIST`); record inos strictly increase across the remount.
   - gen continuity: a snapshot after restore gets a fresh marker id;
     `travel` to a pre-reboot snapshot works; travel-to-base → remount →
     snapshot also gets a fresh id despite the empty live tree.
   - dirty fidelity: snapshot → remount → `IF_CHANGED` snapshot is a no-op;
     staged-but-unsnapshotted work → it isn't.
   - `tests/perm/`: `RESTORE` and `TRAVEL` from inside the mount → `EPERM`.
   - failure path: delete a rename-source in the base while unmounted →
     mount fails with the guidance message, base and artifact intact,
     `yolo review`/`abort` still work unmounted.
   - existing commit/abort tests pass with `RESET` folded into
     `RESTORE(empty)`.
5. `make user`, `make test-vm`.
6. Full parallel-sub-agent code review per AGENTS.md; triage findings.

## Non-goals

- No journal parsing in the kernel — userspace stays the only journal
  reader; the kernel stays the only journal writer while mounted.
- No artifact-lifecycle UX changes: when restore runs, what gets announced,
  and the orthogonal mount/artifact commands are plan 54's scope.
- No locking for concurrent mounts over one `.yolofs/` (same acceptance as
  plan 54).
- No change to rename-redirect representation (plan 56's territory).
