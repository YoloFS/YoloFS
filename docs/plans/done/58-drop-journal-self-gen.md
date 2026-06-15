# 58 — Drop the marker's self-`gen` from the journal

## Status: DONE — implemented and verified (`make test-vm`: 286 unit + 589 e2e pass)

Remove the marker's own generation field (`gen_id`) from the journal wire
format and from the `Marker` type, deriving it from the marker's position
instead. The travel target (`target_gen`) stays — it cannot be derived from
position.

## Background

Three distinct "gen" values exist; this plan touches only the second:

1. `sbi->staging.gen` (kmod runtime, `yolofs.h`) — bumped on every
   snapshot/travel, stamped into inodes to drive re-COW. RAM only;
   re-seeded on mount from the CLI's `latest_gen` via `RESTORE`.
2. **The marker's own `<gen>`** in `P\0<gen>\0<name>` / `T\0<gen>\0<target_gen>`
   = `Marker::gen_id`. **This is what we drop.**
3. `target_gen` in `T` records — which snapshot a travel points to. Stays.

The kernel never reads the journal (`journal.c` header). The self-`gen` is
written by the kernel purely so the CLI can read it, and the CLI already
relies on the invariant `marker[i].gen_id == i` (`marker.rs:35-38`): the
phantom marker 0 sits at index 0 (`core.rs:40`), the kernel increments via
`atomic_inc_return` and never skips, and the journal is append-only
(truncated only on commit/abort, never compacted). So the field is fully
redundant with the marker's index. `timeline.rs:18,24` already prints both
`m_idx` (from `enumerate`) and `gen_id` side by side — they are identical.

## Decision: DROP IT

The marker's self-gen is the single cheapest value in the journal to
reconstruct — it *is* the marker's index, a free loop counter with no
external dependency (unlike `pre`, which on a first touch captures
otherwise-unrecoverable point-in-time base content). Storing it brings no
construction-cost, lookup, or perf benefit; the only thing the stored copy
provides is a cross-check against a parser-dropped/corrupt marker. We judge
that defensive self-check not worth its cost here:

- **`target_gen` stays** (a genuine pointer, not derivable), so removing the
  self-gen doesn't add a new "must trust position" assumption that
  `target_gen` resolution didn't already make.
- Dropping it **simplifies** consumers: `alive_segments` loses its
  `gen → idx` HashMap (target_gen is the index directly) and
  `find_marker_by_gen_id` collapses to a bounds check.
- The journal is append-only and never compacted, and the kernel never skips
  a gen, so position ≡ gen holds by construction.

Accepted trade-off: a parser-dropped marker is no longer detected by a
gen-mismatch; it would silently shift later indices. Acceptable given the
fixed-format, kernel-written, local-fs append-only journal.

## Design

### Wire format

```
P\0<gen>\0<name>\n          →  P\0<name>\n
T\0<gen>\0<target_gen>\n    →  T\0<target_gen>\n
```

The kernel keeps incrementing `staging.gen` exactly as today (still stamped
into inodes, still returned by the SNAPSHOT/TRAVEL ioctls). Only the journal
*write* loses the field.

## Changes

### kmod

**`kmod/journal.c`**

```c
// before
int yolo_journal_snapshot(struct yolo_sb_info *sbi, u16 id, const char *name)
{
	char id_str[6];
	snprintf(id_str, sizeof(id_str), "%u", (unsigned)id);
	return journal_write(sbi, 'P', (const char *[]){ id_str, name, NULL });
}
// after
int yolo_journal_snapshot(struct yolo_sb_info *sbi, const char *name)
{
	return journal_write(sbi, 'P', (const char *[]){ name, NULL });
}
```

```c
// before
int yolo_journal_travel(struct yolo_sb_info *sbi, u16 gen, u16 target_gen)
{
	char gen_str[6], target_str[6];
	snprintf(gen_str, ...); snprintf(target_str, ...);
	return journal_write(sbi, 'T', (const char *[]){ gen_str, target_str, NULL });
}
// after
int yolo_journal_travel(struct yolo_sb_info *sbi, u16 target_gen)
{
	char target_str[6];
	snprintf(target_str, sizeof(target_str), "%u", (unsigned)target_gen);
	return journal_write(sbi, 'T', (const char *[]){ target_str, NULL });
}
```

Update the format comment block at `journal.c:12-13`.

**`kmod/yolofs.h:493-494`** — update the two decls.

**`kmod/ioctl.c`** — callers keep computing `gen`/`new_gen` for the ioctl
return value and inode stamping; only the journal call drops the arg:
- `:408`  `yolo_journal_snapshot(sbi, name_buf);`  (`gen` still set into `snap.gen`)
- `:788`  `yolo_journal_travel(sbi, hdr.target_gen);`  (`new_gen` still used)

### CLI

**`user/journal/types.rs:60-63`**

```rust
pub enum Marker {
    Snapshot { name: String },
    Travel { target_gen: u64 },
}
```

**`user/journal/parse.rs`** — `P` needs `>= 2` fields (name at `[1]`); `T`
needs `>= 2` (target_gen at `[1]`). No self-gen parse. Update the top
format comment (`:9-10`).

```rust
b"P" if fields.len() >= 2 => {
    let name = field_str(fields[1]);
    records.push(Record::Marker(Marker::Snapshot { name }));
}
b"T" if fields.len() >= 2 => {
    if let Ok(target_gen) = String::from_utf8_lossy(fields[1]).parse::<u64>() {
        records.push(Record::Marker(Marker::Travel { target_gen }));
    }
}
```

**`user/journal/core.rs` (`Journal::new`)** — phantom becomes
`Marker::Snapshot { name: "(initial)".into() }`; the marker's gen is its
position (`markers_vec.len()` before the push, since the phantom occupies
index 0):

```rust
Record::Marker(marker @ Marker::Snapshot { .. }) => {
    let gen = markers_vec.len() as u64;
    segments.push(Segment { from: current_from, records: take(&mut current_records) });
    current_from = gen; latest_gen = gen; dirty = false; cow_ino_floor = alloc_ino_floor;
    markers_vec.push(marker);
}
Record::Marker(marker @ Marker::Travel { target_gen }) => {
    let gen = markers_vec.len() as u64;
    segments.push(...);
    markers_vec.push(marker);
    current_from = target_gen; latest_gen = gen; dirty = false; cow_ino_floor = alloc_ino_floor;
}
```

`latest_gen` is now simply the count of P/T markers — what mount feeds back
to `RESTORE`.

**`user/journal/marker.rs`** — the gens become indices:
- `find_marker_by_gen_id(g)` → bounds check only: `(g as usize) < self.0.len()` ⇒ `Ok(g)`, else bail. **(drops the `*g == gen_id` self-check.)**
- `find_snapshot_by_name` → iterate with `.enumerate()`, return the last matching index.
- `marker_at(i)` → `if i == 0 { None } else { self.0.get(i) }`.
- `last_snapshot_idx` → `(i > 0 && matches!(m, Marker::Snapshot { .. }))`.
- `prev_snapshot_idx` → unchanged.
- `alive_segments_range` → delete the `gen_to_idx` map; `target_gen` *is* the
  target index. Replace the map lookup with a bounds guard that reproduces
  the old "miss ⇒ skip": skip when `k_idx >= m` (forward/self ref or
  out-of-range, e.g. the corrupt `target_gen: 99` test) or `k_idx <
  range.start`. Capture `range.start`/`range.end` before consuming `range`.

**Display sites** (print the enumeration index instead of `gen_id`):
- `user/cmd/journal.rs:63,99` — pass the index already in hand
  (`format_marker(seg_idx + 1, marker)`).
- `user/cmd/timeline.rs:24,31` — use the existing `m_idx`.
- `user/cmd/travel.rs:34-38` — use the resolved `target_gen` local for the
  marker's own number; `Travel` label uses its `target_gen` field.

**Unaffected (verified):** `cmd/review.rs:413,428` (numeric gen from the
ioctl return, not a `Marker`), `cmd/snapshot.rs`, `cmd/exec.rs`, `ioctl.rs`
(gen still returned by the kernel).

### Tests (the bulk of the churn)

Every `Marker::Snapshot { gen_id: N, .. }` / `Marker::Travel { gen_id: N, .. }`
literal drops `gen_id` and **must sit at position N** (phantom = 0). Sites:
`journal/marker.rs`, `journal/core.rs`, `journal/parse.rs`, `cmd/journal.rs`,
`cmd/travel.rs`, `tests/internals/test_snapshot.rs`,
`tests/internals/test_travel.rs`, `tests/cli/test_travel.rs`. Update wire
bytes in parse tests (`P\01\0build` → `P\0build`; `T\x004\x002` → `T\x002`).
Add a test that a `T` with an out-of-range `target_gen` is still skipped in
`alive_segments` (replaces the `gen_to_idx` miss path).

### Docs (gate 1 — do these before code)

Update the journal record-format spec and the gen_id-invariant prose in
`docs/architecture.md`, `docs/internals.md`, `docs/staging.md`, and any
`P\0<gen>...` / `T\0<gen>...` lines in `docs/cli.md`.

## Validation

`make test-vm` (unit on host + e2e in VM). The e2e travel/snapshot/timeline
paths exercise the position↔gen mapping end to end.
