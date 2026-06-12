# 61 — Restore protocol: clean failure, explicit re-COW floor, inject tidy

Follow-ups on the RESTORE/TRAVEL wire protocol (user request).

## 1. Auto-quiesce on inject failure

If `yolo_view_inject` fails mid-tree (e.g. base drift while unmounted broke
a rename redirect), the kernel leaves a partially injected view and relies
on the CLI to tear it down. In `yolo_set_view_locked`, re-run
`yolo_staging_quiesce` on inject error so every failed restore/travel
collapses cleanly to base — no half-view window. The travel comment about
not rolling back gen stays true for a different reason: staged inodes may
retain `staging_gen == new_gen` stamps in the icache, so gen must remain
monotonic; the dirent half of that comment becomes obsolete (update it).

## 2. Replace the implicit `gen - 1` stamp with `live_ino_floor`

Restore stamps all injected staged inodes at `gen - 1` so the first
post-remount write re-COWs instead of mutating snapshot-retained content.
Blunt side effect: inodes created in the *live* segment (after the latest
P/T marker — exactly the files being edited before unmount) also re-COW,
leaving a spurious S record + orphan inode per file on first write after
every remount.

Store inos are monotonic, so "live" is a threshold: add a second u32 to
`struct yolo_ioc_restore` (max S-record ino at or before the latest marker;
the CLI's journal walk already tracks the overall max — snapshot it at each
marker). The kernel stamps each injected inode
`ino > floor ? gen : gen - 1` (0-gen guard for the empty/base case).
TRAVEL passes floor 0 — every ino > 0 stamps at the new gen, preserving
travel's write-in-place behavior. `yolo_set_view_locked` takes
(gen, floor) instead of (gen, inode_gen).

Naming (user request): both fields are "max S-record ino" thresholds and are
named for purpose, as a symmetric pair — `alloc_ino_floor` (was `max_ino`;
allocation resumes above it — dead/deleted inos still occupy the store) and
`cow_ino_floor` (the new field; write-in-place only above it). They coincide
exactly when the live segment has no S records, and neither is derivable
from the tree (it omits inos of files later deleted or renamed-over).

Mirror the struct in user/ioctl.rs (+ struct_sizes pin), compute the floor
in user/journal/core.rs, pass it in cmd/mount.rs (abort/commit pass 0 with
their empty trees).

Tests (tests/internals/): after snapshot → unmount → remount → write, the
file still re-COWs (new S record, new ino); after edit-without-snapshot →
unmount → remount → write, no re-COW (no new S record).

## 3. Inject loop: stop re-looking-up just-created children

`travel_inject_entry` creates the child via `yolo_dentry_create` (which
returns the dentry), discards it, and the descend step re-finds it with
`lookup_one_len_unlocked`. Return the dentry instead (NULL for passthrough
scaffolds, which create none); descend dgets the returned child and only
falls back to the lookup for scaffolds. One hash lookup saved per directory
node, one error path removed.

## 4. Hygiene

- ioctl.c comment claims "Scaffold (tag 0) entries have no payload" — tag 0
  is -EINVAL; scaffolds are TARGET_PATH with path_len == 0. Fix.
- user/ioctl.rs `travel()`: the empty-buffer `tree_ptr = 0` special case is
  dead — an empty DirTree serializes to 2 bytes (root count). Drop it.
- Document the restore struct fields' semantics in yolofs.h.

Considered, not done: a version byte on the tree buffer (no compat
requirement; struct_sizes pins layout at build time); kernel-persisted
gen/dirty/next_ino state file (breaks the journal-as-single-source design).

Docs: update the restore paragraph in docs/staging.md (stamping policy) to
match. Verify with make kmod, make test-vm, parallel review.
