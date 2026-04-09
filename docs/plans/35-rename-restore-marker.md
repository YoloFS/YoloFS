# 35 — Rename Marker→Meta, Checkpoint→Mark, Restore→Jump, K→M, T→J

## Problem

The `Marker` type name is too generic — "meta" better describes control
metadata records.  The internal record names should use short, distinct
terms: "mark" (M) for checkpoint records and "jump" (J) for restore records.

## Naming convention

| | User-facing (CLI) | Internal (code/journal) |
|---|---|---|
| Bookmark a point | `yolo checkpoint` | Mark (`M` tag, `Meta::Mark`) |
| Return to a point | `yolo restore` | Jump (`J` tag, `Meta::Jump`) |

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
| Kernel ioctl | `YOLO_IOC_CHECKPOINT` | `YOLO_IOC_MARK` |
| Kernel struct | `yolo_ioc_checkpoint` | `yolo_ioc_mark` |
| Kernel fn | `yolo_journal_checkpoint` | `yolo_journal_mark` |
| Rust ioctl | `ioctl::create_checkpoint()` | `ioctl::mark()` |
| Rust struct | `YoloIocCheckpoint` | `YoloIocMark` |

### 3. `Restore` → `Jump`, `T` → `J`

| Layer | Before | After |
|---|---|---|
| Journal tag | `T` | `J` |
| Enum variant | `Marker::Restore` | `Meta::Jump` |
| Kernel ioctl | `YOLO_IOC_RESTORE` | `YOLO_IOC_JUMP` |
| Kernel struct | `yolo_ioc_restore` | `yolo_ioc_jump` |
| Kernel fn | `yolo_journal_restore` | `yolo_journal_jump` |
| Rust ioctl | `ioctl::restore()` | `ioctl::jump()` |
| Rust struct | `YoloIocRestore` | `YoloIocJump` |

CLI command stays as `yolo restore`, file renamed to `cmd/restore.rs`.

### 4. Legacy `M` tag removed

The parser previously accepted `M` as a legacy alias for `A` (Add) records.
This was removed to free the `M` tag for Mark records.
