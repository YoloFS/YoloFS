# 35 — Rename Marker→Meta, Checkpoint→Mark, Restore→Jump, K→M, T→J

## Problem

The `Marker` type name is too generic — "meta" better describes control
metadata records.  The internal record names should use short, distinct
terms: "mark" (M) for checkpoint records and "jump" (J) for restore records.

## Naming convention

| | User-facing (CLI) | Internal (code/journal) |
|---|---|---|
| Bookmark a point | `agfs checkpoint` | Mark (`M` tag, `Meta::Mark`) |
| Return to a point | `agfs restore` | Jump (`J` tag, `Meta::Jump`) |

CLI commands, help text, and general user-visible output use "checkpoint" and
"restore". Internal types, journal tags, kernel functions, and ioctl names
use "mark" and "jump". Paper benchmark figures may use presentation-specific
labels such as "snapshot" and "Base" when that improves figure readability.

## Renames

### 1. `Marker` → `Meta`, `Markers` → `Metas`

| Before | After |
|---|---|
| `enum Marker` | `enum Meta` |
| `Record::Marker(m)` | `Record::Meta(m)` |
| `struct Markers` | `struct Metas` |
| `journal.markers` field | `journal.metas` |
| `markers.rs` file | `metas.rs` |
| `find_marker()` | `find_meta()` |
| `marker_at()` | `meta_at()` |

### 2. `Checkpoint` → `Mark`, `K` → `M`

| Layer | Before | After |
|---|---|---|
| Journal tag | `K` | `M` |
| Enum variant | `Marker::Checkpoint` | `Meta::Mark` |
| Kernel ioctl | `AGFS_IOC_CHECKPOINT` | `AGFS_IOC_MARK` |
| Kernel struct | `agfs_ioc_checkpoint` | `agfs_ioc_mark` |
| Kernel fn | `agfs_journal_checkpoint` | `agfs_journal_mark` |
| Rust ioctl | `ioctl::create_checkpoint()` | `ioctl::mark()` |
| Rust struct | `AgfsIocCheckpoint` | `AgfsIocMark` |

### 3. `Restore` → `Jump`, `T` → `J`

| Layer | Before | After |
|---|---|---|
| Journal tag | `T` | `J` |
| Enum variant | `Marker::Restore` | `Meta::Jump` |
| Kernel ioctl | `AGFS_IOC_RESTORE` | `AGFS_IOC_JUMP` |
| Kernel struct | `agfs_ioc_restore` | `agfs_ioc_jump` |
| Kernel fn | `agfs_journal_restore` | `agfs_journal_jump` |
| Rust ioctl | `ioctl::restore()` | `ioctl::jump()` |
| Rust struct | `AgfsIocRestore` | `AgfsIocJump` |

CLI command stays as `agfs restore`, file renamed to `cmd/restore.rs`.

### 4. Legacy `M` tag removed

The parser previously accepted `M` as a legacy alias for `A` (Add) records.
This was removed to free the `M` tag for Mark records.
