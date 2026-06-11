# 59 — kmod cleanups: dead code, dedup, readdir/lookup optimization, shard cache race

Findings from a full read of `kmod/`. All changes are kernel-internal
except §1b, which renames one journal letter (kernel + userspace +
`docs/staging.md`); none of the other touched internals are described in
the design docs.

## 1. Dead code / mechanical simplifications

1. **file.c** — remove `yolo_check_open_perm()` (its `buf` parameter is
   unused) and the 256-byte `buf[YOLO_PATH_MAX]` in `yolo_open()`; call
   `yolo_check_dentry_perm()` directly.
2. **inode.c** — drop the `yolo_unlink`/`yolo_rmdir` wrappers; point both
   ops at `yolo_delete_entry` (signatures already match). Hoist the
   mid-function declarations in `yolo_delete_entry`/`yolo_rename` to the top.
3. **inode.c** — collapse the `yolo_permission()` switch: `ALLOW`, `ASK`,
   and `WRITE_ASK` all return 0 unconditionally (asks resolve in open/
   metadata paths); only `READ_ONLY`+write, `DENY`, and unexpected values
   fall through to `-EACCES`. Semantics unchanged.
4. **perm.c** — merge the byte-identical `WRITE_ASK`/`READ_ONLY` cases in
   `yolo_check_perm()`; drop the zero-value field assignments after
   `kzalloc` in `yolo_ask_userspace()`.
5. **journal.c** — replace `decision_char()` (dead error path: both callers
   pass a validated enum) with a ternary, mirroring `op_char()`.
6. **ioctl.c** — deduplicate the two near-identical deny loops in
   `yolo_ctl_release()` into one helper (`put_ref` distinguishes the
   dispatched list, which carries the extra GET_ASK reference). Still runs
   under `pending_lock` — completing under the lock is what serializes
   against the requester's settle path.
7. **dir.c** — remove the `extern file_dentry` declaration (static inline
   in `linux/fs.h` since v4.6).

## 1b. Journal ask-decision letter: `y`/`d` → `y`/`n`

(Added at user request.) The A-record decision letter pair mixed
vocabularies — `y` answers the ask, `d` names the verdict — and `d` visually
collides with the `D` record tag (`a` for allow would collide with the
Absence pre-target tag). Switch deny to `n` so the field reads as the yes/no
answer to the ask. Touches `kmod/journal.c` (`decision_char`),
`user/perm.rs` (`to_letter`/`from_letter` + unit tests), comments in
`user/journal/parse.rs` + its raw-bytes test, and `docs/staging.md`. No
backward compat needed per AGENTS.md; old `d` records simply parse as
skipped.

## 2. Shared perm-refresh helper

The "re-cache perm if `perm_gen` is stale, then read `cached_perm`" pattern
is repeated in `yolo_getattr`, `yolo_dir_open`, and
`yolo_check_dentry_perm`. Add `yolo_effective_perm(inode, dentry)` in
perm.c and use it at all three sites. (`yolo_permission()` keeps its own
variant — it only has an inode and must go through `d_find_alias`.)

## 3. Readdir phase-2 hidden check: O(children) scan → d_lookup

`yolo_fill_base()` does a `d_lookup` to test for pinned overrides, then
*separately* walks the parent's entire `d_children` list per base entry to
test for `YOLO_PERM_HIDE` rules — O(entries × children). Merge both into
one `d_lookup`: skip when `pinned || (perm.enabled && perm == HIDE)`.

Safety: explicit-rule dentries are resolved via `kern_path` at rule-set
time, pinned with `dget`, and stay hashed (the `d_drop` in `yolo_lookup`
only hits freshly looked-up dentries, which by definition were not in the
dcache; a deleted name is covered by its pinned tombstone via the same
lookup). Covered by `tests/perm/test_hide.rs` (`readdir_skips_hidden_entry`,
`hide_single_file`) and `tests/fs/test_readdir.rs`.

## 4. lookup.c: lookup_one_len_unlocked

Replace manual `inode_lock_shared` + `lookup_one_len` + unlock with
`lookup_one_len_unlocked()` (already used in ioctl.c), which tries a
lockless dcache hit first.

## 5. Bug fix: shard cache race in get_shard_dir()

`sbi->staging.shard_dentry`/`shard_id` are read and replaced with no lock.
The COW path holds `staging.sem`, but the create path
(`yolo_create_staged` → `yolo_inode_alloc`) takes only the parent dir's
`i_rwsem`, which does not serialize creates in *different* directories.
Two concurrent creates crossing a shard boundary can both replace the
cache and double-`dput` the old dentry.

Fix: add `spinlock_t shard_lock` to `struct yolo_staging` guarding the two
cache fields. `dget` under the lock; `dput` of the displaced entry outside
it. `yolo_staging_quiesce()` (ioctl.c) takes the same lock when clearing
the cache — it holds `staging.sem` for write, but creates don't take
`staging.sem` at all. `yolo_put_super` runs with no concurrent ops and
stays as-is.

Per user direction, no new test for this race (a deterministic reproducer
isn't practical); existing concurrency tests exercise the path.

## 6. Review follow-ups

From the parallel code review of the change set:

- **Sticky shard-cache invalidation** (bugs review, medium): the spinlock in
  §5 fixed the pointer race but a create racing past quiesce could still
  re-publish its stale shard dentry into the cache afterwards. Add
  `shard_epoch` (under `shard_lock`), bumped by quiesce; `get_shard_dir`
  captures it before the slow-path lookup and skips the cache publish if it
  changed.
- **Quality**: hoist the `alias` declaration in `yolo_permission`; move the
  `fi` allocation in `yolo_open` after the perm gate (denied opens no longer
  pay an alloc/free); comment the intentionally unlocked `di->perm` read in
  `yolo_base_entry_skipped`; fix the comment banner split in ioctl.c.
- **Docs**: retitle staging.md's delete pseudocode to `yolo_delete_entry`
  (the wrappers it named are gone), add the hide-skip to its phase-2
  readdir pseudocode, update stale wrapper names in test file comments.
- **Tests**: two readdir-hide gaps now covered in `tests/perm/test_hide.rs`:
  a hidden base entry alongside staged/tombstone/overridden siblings
  (phase-1/phase-2 interaction), and a live `rule hide` followed by
  stat-then-readdir rounds (rule dentry must stay findable by `d_lookup`).
- Accepted without action: old journals' `d` decision letters parse as
  skipped (no back-compat per AGENTS.md); the unlocked `di->perm` read
  stays a plain read to match `yolo_resolve_perm`.

## Verification

- `make kmod` builds clean.
- `make test-vm` (unit + e2e in VM) passes.
- Full parallel code review per AGENTS.md before finalizing.
