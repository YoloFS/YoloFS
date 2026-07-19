# 74 — Rename the content-location "target" concept to "backing"

## Goal

The type that records *where an overlay name's content lives* is named
`target` in code but "backing" in every comment and doc. The word `target` is
also overloaded: it separately names *the dentry an operation or rule acts on*
(`yolo_journal_gate(@target)`, `yolo_perm_check_dentry(@target)`, the ioctl
rule target). Rename the content-location concept to `backing` so the types
match the prose and stop colliding with the operation/rule `target`.

Pure rename — no behavior, wire-format, or data-model change.

## Naming

Kernel (`enum yolo_target` → `enum yolo_backing`):

| Old | New |
|-----|-----|
| `enum yolo_target` | `enum yolo_backing` |
| `YOLO_TARGET_INODE` | `YOLO_BACKING_STAGED` |
| `YOLO_TARGET_PATH` | `YOLO_BACKING_BASE` |
| `YOLO_TARGET_NONE` | `YOLO_BACKING_NONE` |
| `yolo_dentry_info.target` | `.backing` |
| `yolo_preimage_target()` | `yolo_preimage_backing()` |

Userspace (`enum Target` → `enum Backing`):

| Old | New |
|-----|-----|
| `Target` | `Backing` |
| `Target::Absence` | `Backing::None` |
| `Target::StagedFile(u32)` | `Backing::StagedFile(u32)` |
| `Target::BasePath(String)` | `Backing::BasePath(String)` |

Rationale for the variant names:
- Kernel variants move from addressing mechanism (`INODE`/`PATH`) to the
  backing concept (`STAGED`/`BASE`), aligning with userspace and the "what
  backs this name" framing. The mechanism stays documented in comments.
- `NONE`/`None` unifies the empty case across both sides.
- `StagedFile`/`BasePath` keep their suffixes: the base backing can be a
  directory (ground state via `d_path` on the lower path), so `BasePath` is
  more accurate than `BaseFile`, and the payload is a path string matching the
  `b:<path>` wire tag.

## Out of scope (deliberately unchanged)

- Operation/rule `@target` parameters (`yolo_journal_gate`,
  `yolo_perm_check_dentry`, ioctl rule target) — disambiguating these from the
  content-location concept is the point of the rename.
- The wire format (`a` / `s:<ino>` / `b:<path>` tags) and journal record tags.
- The data model: `staging_ino`/`staging_gen`/`perm_gen` stay on the inode
  (the `->permission` hook receives an inode and reads the cache without a
  dentry; the staged id is content identity). Only `backing` (the kind) lives
  on the dentry, by design.

## Steps

1. Kernel: rename the enum, constants, struct field, and
   `yolo_preimage_target`; the compiler flags every stale reference.
2. Userspace: rename the enum and `Absence`→`None`; the compiler and tests
   flag stale references.
3. Docs: update `staging.md`, `permissions.md`, `architecture.md` prose and
   tables that name `Target`/`INODE`/`PATH`/`Absence`.
4. `make kmod`, `cargo build`, `make test-unit`; hand off `make test` for the
   host to run the e2e/VM suite.
5. Full parallel code review, then archive this plan.
