# Append-Only Journal with Restore Records

Replace journal truncation on restore with an append-only design. The journal
becomes both the reconstruction log and the audit trail — a single source of
truth written exclusively by the kernel.

---

## 1. Motivation

`agfs restore` currently truncates the journal to the target checkpoint,
destroying the history of post-checkpoint work. This loses the audit trail
(what was tried, what was undone) and requires byte-offset tracking in the
parser solely for truncation.

An append-only journal is simpler, preserves full session history, and enables
future caching optimizations without changing the on-disk format.

---

## 2. Design

### 2.1 New journal record: `S` (restore)

```
S\0<gen>\0<target_gen>\n
```

| Field | Meaning |
|-------|---------|
| `gen` | The new generation assigned to this restore (monotonically increasing) |
| `target_gen` | The checkpoint being restored to |

Written by the kernel during `AGFS_IOC_RESTORE` (when `entry_count > 0`).
The `S` record acts as both a restore marker and a segment boundary.

### 2.2 Generation stays monotonic

Currently, restore resets `sbi->checkpoint_gen` to the target checkpoint's
value. This causes checkpoint ID collisions when new checkpoints are created
after restore.

New behavior: restore **increments** gen (like checkpoint does). Injected
dirents receive the new gen value:

```
After restore to K2 (gen bumped from 3 → 4):
  dirent.gen = 4, sbi->gen = 4  →  no spurious re-COW  ✓
After next checkpoint (gen 5):
  dirent.gen = 4, sbi->gen = 5  →  re-COW triggers      ✓
```

IDs in the journal are always unique and increasing:
```
K1 [A] K2 [B] K3 S4(K2) [D] K5
                  ↑ gen=4, no collision with dead K3
```

### 2.3 Naming unification: `gen`

The same monotonically increasing counter serves as both the checkpoint
identifier and the re-COW generation. Unify the naming:

| Where | Before | After |
|-------|--------|-------|
| Kernel sbi field | `checkpoint_gen` | `gen` |
| Kernel dirent field | `checkpoint_gen` | `gen` |
| Ioctl struct fields | `checkpoint_gen` | `target_gen` / `new_gen` |
| Journal CKP record | `K\0<id>\0<name>\n` | `K\0<gen>\0<name>\n` |
| Rust `Checkpoint` struct | `id: u64` | `gen_id: u64` |
| Docs | "checkpoint ID" / "generation counter" | "gen" |

### 2.4 Ioctl struct change

```c
// Before (write-only)
#define AGFS_IOC_RESTORE  _IOW('A', 41, struct agfs_ioc_restore)
struct agfs_ioc_restore {
    __u64 checkpoint_gen;     // in: set gen to this value
    __u64 entry_count;
    __u64 entries_ptr;
};

// After (read-write, to return new_gen)
#define AGFS_IOC_RESTORE  _IOWR('A', 41, struct agfs_ioc_restore)
struct agfs_ioc_restore {
    __u64 target_gen;         // in: checkpoint gen to restore to (0 = reset)
    __u64 new_gen;            // out: new generation assigned
    __u64 entry_count;
    __u64 entries_ptr;
};
```

Kernel behavior by `target_gen`:

| `target_gen` | `entry_count` | Mode | Behavior |
|-------------|---------------|------|----------|
| `0` | `0` | Reset (commit/abort) | Wipe dirents, set gen=1, no RST record |
| `> 0` | `≥ 0` | Restore | Wipe dirents, inject entries, increment gen, stamp dirents with new gen, append RST record, return new_gen |

Restore to a named checkpoint (`target_gen>0`, `entry_count≥0`) is distinct
from commit/abort (`target_gen=0`) by the target_gen value.

### 2.5 Reachable record extraction

S records create unreachable records — journal records between the target checkpoint
and the RST record that no longer reflect the current state. All consumers
(commit, status, diff, restore) must filter these out before resolving.

**Algorithm** — O(N) single pass + O(R) backward walk:

```
reachable(records):
    // Pass 1: collect S and K positions — O(N)
    s_list = [(pos, target_gen) for each RST record]
    k_map  = {gen: pos for each CKP record}

    // Pass 2: walk RST records right-to-left — O(R)
    ranges = []
    end = len(records)

    for s_pos, target_gen in reversed(s_list):
        if s_pos >= end: continue          // S in unreachable region, skip
        if s_pos + 1 < end:
            ranges.push((s_pos + 1, end))  // live suffix after this S
        end = k_map[target_gen] + 1        // narrow to target K

    ranges.push((0, end))                  // live prefix up to final target
    ranges.reverse()
    return concat(records[start..end] for (start, end) in ranges)
```

After extraction, the output contains only ADD/MOD/DEL/RDR/CKP records. All downstream
logic (`resolve`, `resolve_segments`, `slice_records`) works unchanged.

**Worked example:**

```
idx:  0   1  2  3  4  5       6  7   8  9  10
      K1 [A] K2 [B] K3 S4(K1) [D] K5 [E] K6 S7(K5)

s_list = [(5, gen=1), (10, gen=5)]
k_map  = {1:0, 2:2, 3:4, 5:7, 6:9}
end    = 11

S7 (pos=10): 10 < 11 ✓, 11 == 11 so no suffix, end = 7+1 = 8
S4 (pos=5):   5 <  8 ✓, push (6,8),               end = 0+1 = 1
push (0,1)

ranges = [(0,1), (6,8)]  →  [K1], [[D], K5]
reachable records: K1, [D], K5  ✓
```

### 2.6 Restore target lookup

Restore must find the target checkpoint in **all** records (not just live
ones), because the target might be in an unreachable region (undo-restore). After
finding the target, it runs `reachable` on the prefix `records[0..=target]`
and resolves the result.

For `--at`/`--from`/`--to` (used by status/diff), checkpoints are searched
in reachable records only. Unreachable checkpoints are not addressable for display.

### 2.7 `agfs log` — full audit trail

`agfs log` reads **all** records (no `reachable`) and displays events
chronologically:

```
[1] after make build
[2] after make test
[3] restored to [1]
[4] after make fix
```

All other commands (status, diff, commit, restore) work on reachable records only.

### 2.8 Who writes what

| Actor | Reads journal | Writes journal |
|-------|---------------|----------------|
| Kernel | Never | Always (ADD/MOD/DEL/RDR/REP/CKP/RST via `kernel_write`) |
| CLI | Yes (resolve, status, diff, commit) | Never (except `set_len(0)` on commit/abort to clear) |

The journal is append-only within a session. Commit and abort are session
boundaries that clear everything (journal + inodes + caches).

---

## 3. Caching (deferred optimization)

The core design works without caching. These optimizations avoid re-processing
the full journal on each CLI invocation.

### What to cache (in `.agfs/cache/`)

| Cache | Key | Value | Benefit |
|-------|-----|-------|---------|
| S/K positions | journal byte length | `Vec<(pos, gen, target_gen)>` for S, `Vec<(pos, gen)>` for K | Incremental reachable range extraction — avoid full journal scan |
| Resolver state | checkpoint gen | Serialized `BTreeMap<String, Action>` | O(unsaved) commit/restore instead of O(all reachable records) |
| Segment changes | `(from_gen, to_gen)` | Serialized `Vec<Change>` | O(trailing) status/diff instead of O(all reachable records) |

### Cache properties

- **Immutable by design**: checkpoint caches never change (no open fds during
  checkpoint = no mutations can slip in).
- **No invalidation needed for RST records**: unreachable caches just go unused.
- **Commit/abort clears all caches** (session boundary).
- **Eager build**: compute and persist caches right after `agfs checkpoint`
  returns.

### With warm caches

| Operation | Work (no cache) | Work (warm cache) |
|-----------|----------------|-------------------|
| `agfs status` / `agfs diff` | O(N) parse + extract + resolve | O(trailing segment only) |
| `agfs commit` | O(N) parse + extract + resolve | O(unsaved records only) |
| `agfs restore` | O(target prefix) parse + extract + resolve | O(1) load cached state |

---

## 4. Implementation Plan

### Phase 1: Documentation

| ID | Task | Files |
|----|------|-------|
| docs-staging | RST record format, append-only semantics, `reachable` algorithm, gen naming | `docs/staging.md` |
| docs-internals | RESTORE ioctl changes (`_IOWR`, `target_gen`/`new_gen`, RST record write), gen naming | `docs/internals.md` |
| docs-cli | `agfs log` shows restore events, gen naming | `docs/cli.md` |
| docs-architecture | Lifecycle example with restore | `docs/architecture.md` |

### Phase 2: Kernel module

| ID | Task | Files | Depends on |
|----|------|-------|------------|
| kmod-rename-gen | Rename `checkpoint_gen` → `gen` | `kmod/agfs.h`, `kmod/*.c` | docs-staging, docs-internals |
| kmod-journal-restore | Add `agfs_journal_restore()` for RST records | `kmod/journal.c` | docs-staging, docs-internals |
| kmod-ioctl-restore | Modify RESTORE handler: `_IOWR`, increment gen, write S, return `new_gen` | `kmod/ioctl.c`, `kmod/agfs.h` | kmod-rename-gen, kmod-journal-restore |
| kmod-ioctl-checkpoint | Update CHECKPOINT for gen naming | `kmod/ioctl.c` | kmod-rename-gen |

### Phase 3: CLI core

| ID | Task | Files | Depends on |
|----|------|-------|------------|
| cli-rename-gen | Rename `checkpoint_gen` → `gen` in CLI | `cli/*.rs` | kmod-rename-gen |
| cli-journal-parse-S | Add `Record::Restore`, parse S tag, remove offset tracking | `cli/journal.rs` | kmod-journal-restore |
| cli-reachable | Add `reachable()` — O(N) unreachable record removal | `cli/resolve.rs` | cli-journal-parse-S |
| cli-ioctl-struct | Update `AgfsIocRestore` struct, `_IOWR`, return `new_gen` | `cli/ioctl.rs` | kmod-ioctl-restore |

### Phase 4: CLI commands

| ID | Task | Files | Depends on |
|----|------|-------|------------|
| cli-restore | Remove truncation, use `reachable` on prefix, pass `target_gen` | `cli/restore.rs` | cli-reachable, cli-ioctl-struct |
| cli-commit | Call `reachable` before `resolve` | `cli/commit.rs` | cli-reachable |
| cli-diff-status | Call `reachable` before `slice_records`/`resolve_segments` | `cli/diff.rs` | cli-reachable |
| cli-log | Show RST records in `agfs log` | `cli/checkpoint.rs` | cli-journal-parse-S |
| cli-find-checkpoint | All records for restore targets, reachable records for `--at`/`--from`/`--to` | `cli/resolve.rs` | cli-reachable |

### Phase 5: Tests

| ID | Task | Depends on |
|----|------|------------|
| test-reachable | Unit tests: no S, single restore, multiple restores, unreachable S, undo restore | cli-reachable |
| test-resolve-with-S | Unit tests: `resolve()` produces correct changes with RST records in input | cli-reachable |
| test-segments-with-S | Unit tests: `resolve_segments` correct segment boundaries after `reachable` | cli-reachable |
| test-e2e-restore | E2E: restore + work + commit, multiple restores, undo restore, `agfs log` audit trail | cli-restore, cli-commit, cli-diff-status, cli-log |

### Phase 6: Caching (deferred)

| ID | Task | Depends on |
|----|------|------------|
| cache-infrastructure | `.agfs/cache/` directory, read/write helpers | test-e2e-restore |
| cache-positions | Cache S/K positions for incremental reachable range computation | cache-infrastructure |
| cache-resolver-state | Cache resolver `BTreeMap` at checkpoint boundaries | cache-infrastructure |
| cache-segments | Cache per-segment `Vec<Change>` | cache-infrastructure |
| cache-integration | Integrate caches into resolve pipeline | cache-positions, cache-resolver-state, cache-segments |
