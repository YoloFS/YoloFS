# Architecture

YoloFS stacks on top of any local lower filesystem (ext4, xfs, btrfs, ...) using VFS
interposition. It adds two orthogonal capabilities:

| Capability            | Summary |
| --------------------- | ------- |
| **Staging-commit**    | Every write goes to a staging layer. Changes are invisible to the lower FS until an explicit `commit`. An `abort` discards them instantly. |
| **Permission gating** | Every file starts in the `ask` state. A rule engine promotes matching paths to `allow`, `write-ask`, `read-only`, `ask`, `deny`, or `hide`. When an access needs approval (`ask`, or a write under `write-ask`), the thread is put to sleep; a userspace daemon receives the request and writes back a decision that wakes the thread. |

## Design Goals

- **In-kernel, zero-copy data path** — no FUSE overhead, no context switches
  for allowed operations.
- **Unprivileged mounting** via user namespaces (same as current YoloFS).
- **Composable** — staging and permission gating are independent layers;
  either can be disabled at mount time.

## Two-Layer Architecture

```
 ┌──────────────────────────────────────────────────┐
 │                   User Process                    │
 │              (AI agent / shell / ...)             │
 └────────────────────┬─────────────────────────────┘
                      │ VFS syscall
 ┌────────────────────▼─────────────────────────────┐
 │                    YoloFS                           │
 │       ┌─────────────┐    ┌──────────────┐        │
 │       │ Perm Gating │ →  │   Staging    │        │
 │       │   Layer     │    │    Layer     │        │
 │       └─────────────┘    └──────────────┘        │
 └────────────────────┬─────────────────────────────┘
                      │ vfs_*() on lower FS
 ┌────────────────────▼─────────────────────────────┐
 │              Lower filesystem (ext4 ...)          │
 └──────────────────────────────────────────────────┘

 ┌──────────────────────────────────────────────────┐
 │  ioctl on mount-root directory fd                  │
 │    ← YOLO_IOC_GET_ASK:  dequeue an ask           │
 │    → YOLO_IOC_PUT_DECISION: post decision        │
 │    → YOLO_IOC_RULE_SET/RESOLVE: manage rules     │
 │    → YOLO_IOC_RESET: reset staging               │
 │    → YOLO_IOC_TRAVEL: travel                     │
 │    → YOLO_IOC_SNAPSHOT: create snapshot          │
 └──────────────────────────────────────────────────┘
```

The two layers execute in order for every VFS operation:

1. **Perm Gating Layer** — resolves the effective permission for the path.
   Regular-file opens use the file's own permission. Directory
   mutations use the parent directory's permission. If `ask`, the
   thread sleeps until userspace decides. Directory read-like ops
   (lookup, traversal, readdir, stat) are **not** gated — only `hide`
   returns `-ENOENT`; `deny`/`ask` have no effect on them.
2. **Staging Layer** — routes reads to the staged inode if the file has been
   modified, otherwise to the base. Ensures writes go to staged inodes.
   Uses per-directory linked lists of pinned staged VFS dentries for deletions and renames.

All I/O is ultimately delegated to the lower filesystem via `kiocb` swapping
and `vfs_*()` calls.

## Why Stackable VFS?

- **Portability**: Works on any local underlying filesystem (ext4, xfs, btrfs, tmpfs).
- **File-level granularity**: Permission gating operates on files and
  directories, which map naturally to inodes in a stackable FS.
- **Simplicity**: No need to manage block allocation, journaling, or
  filesystem metadata. The lower FS handles all of that.

## Comparison with OverlayFS

YoloFS uses a fundamentally different staging model from OverlayFS.

**Staging vs live union**: OverlayFS is a live union filesystem — the upper
layer *is* the persistent state. There is no commit or abort. A renamed
file is copied up to upper with `RENAME_WHITEOUT` and stays there forever.
YoloFS treats staging as a flat inode store with in-memory staging state
(staging state fields (`target`, `pinned`) on pinned VFS dentries) that is explicitly committed or
discarded via the journal.

**Copy-up**: OverlayFS always does a full copy-up on first write, even for
truncating writes (`echo "x" > file` copies the entire file, then
truncates). YoloFS detects `O_TRUNC` and creates an empty staged inode
directly — zero copy for the most common agent write pattern.

**Rename**: OverlayFS does a real `vfs_rename()` in the upper directory,
which requires copy-up. YoloFS does zero-copy renames by setting packed state
on VFS dentries (negative dentry on old parent, redirect dentry on new parent).
Rename chains resolve naturally through the dcache.

**Lookup**: OverlayFS does two lookups per component (upper + lower) and
merges the results. YoloFS checks the VFS dcache first (staged entries are
pinned), then falls back to base — one lookup.

**Permission model**: OverlayFS uses standard Unix permissions only. YoloFS
adds the progressive gating layer (`allow`, `write-ask`, `read-only`, `ask`,
`deny`, `hide`) with the ask protocol for interactive approval.

**On-disk format**: OverlayFS requires filesystem support for whiteouts
(`RENAME_WHITEOUT`, ext4/xfs). YoloFS uses a flat inode store + append-only
journal, working on any lower FS. The journal uses typed record tags
(`S`/`D`/`R` for mutations, `P`/`T` for snapshots/travels) so
each record is self-describing. All renames — staged or redirect — emit a
single R record carrying both source and destination paths.

## Lifecycle Example

```
# 1. Full interactive workflow (mount -> watch + run -> review -> commit/abort)
$ cd /home/user/project
$ yolo
   -> creates .yolofs/, mounts / -> .yolofs/mnt, applies rules from yolofs.toml,
     starts background watch daemon for permission requests, chroots into
     .yolofs/mnt, spawns $SHELL with cwd preserved as the caller's original CWD
   -> on shell exit: stops watch daemon, runs `yolo review`, prompts user to
     commit, abort, or keep staged (user runs `yolo unmount` when done)

# 1b. Or use individual commands for more control:
$ yolo mount
$ yolo watch &           # start daemon in background
$ yolo run -- make build
$ yolo review
$ yolo commit

# 1c. Install rules via CLI from the session root (attaches perm directly
#     to dentries)
$ yolo rule allow src
$ yolo rule write-ask /etc
$ yolo rule read-only /etc/hosts

# 2. Agent writes to a file matching an allow rule
$ echo "hello" > /src/main.rs
   -> kernel: yolo_lookup("src") -> explicit rule on dentry -> perm=ALLOW
   -> kernel: yolo_lookup("main.rs") -> no rule on dentry (UNSET)
              -> yolo_cache_perm() walks up: main.rs(UNSET) -> src(ALLOW)
              -> caches ALLOW on main.rs inode
   -> kernel: yolo_open() -> cached_perm=ALLOW, O_WRONLY -> pass
   -> kernel: yolo_write_iter() -> pass-through to staged inode

# 3. Agent reads /etc/passwd (readable -- /etc has write-ask rule)
$ cat /etc/passwd
   -> kernel: yolo_lookup("etc") -> explicit rule on dentry -> perm=WRITE_ASK
   -> kernel: yolo_lookup("passwd") -> no rule on dentry (UNSET)
              -> yolo_cache_perm() walks up: passwd(UNSET) -> etc(WRITE_ASK)
              -> caches WRITE_ASK on passwd inode
   -> kernel: yolo_open("passwd") -> cached_perm=WRITE_ASK, O_RDONLY -> pass

# 4. Agent reads /etc/hosts (explicit override -> read-only)
$ cat /etc/hosts
   -> kernel: yolo_lookup("hosts") -> explicit rule on dentry -> perm=READ_ONLY
              -> yolo_cache_perm() -> caches READ_ONLY on hosts inode
   -> kernel: yolo_open() -> cached_perm=READ_ONLY -> pass

# 5. Agent reads /tmp/secrets (no rule anywhere -> walk up reaches root -> ask)
$ cat /tmp/secrets
   -> kernel: yolo_lookup("tmp") -> no rule on dentry (UNSET)
   -> kernel: yolo_lookup("secrets") -> no rule on dentry (UNSET)
              -> yolo_cache_perm() walks up: secrets(UNSET) -> tmp(UNSET) -> root(UNSET)
              -> no rule found, caches built-in default ASK on secrets inode
   -> kernel: yolo_open() -> cached_perm=ASK
   -> kernel: enqueue request, thread sleeps
   -> daemon: ioctl(GET_ASK) -> yolo_ioc_ask { id:1, access_path:"/tmp/secrets",
                                               rule_path:"", rule_perm:ASK, ... }
   -> daemon: decision: allow
   -> daemon: ioctl(PUT_DECISION, yolo_ioc_decision { id:1, decision:ALLOW })
   -> kernel: wake thread, allow this read once

# 6. Agent tries to write /etc/hosts (walk up finds READ_ONLY)
$ echo x >> /etc/hosts
   -> kernel: yolo_open() -> READ_ONLY, O_WRONLY -> -EACCES

# 7. Commit all staged changes to the real filesystem (userspace)
$ yolo commit
   -> userspace: replay journal -- apply renames, deletes, move inodes to base
   -> userspace: ioctl(YOLO_IOC_RESET) on .yolofs/mnt
   -> kernel: release staged dentries, invalidate dentry + inode caches
   -> umount .yolofs/mnt

# 8. Travel to a previous marker (appends T record, no truncation)
$ yolo travel "after make build"
   -> CLI: Journal → find_marker → live_segments_at_name → build tree → serialize tree
   -> CLI: ioctl(YOLO_IOC_TRAVEL, { target_gen=2, tree_buf })
   -> kernel: release staged dentries, inject VFS dentries from tree, increment gen to 4,
      append T record to journal
   -> journal is append-only — dead records remain but are filtered
      by Journal reachability on subsequent operations
```

## Source File Layout

```
yolofs/
├── README.md
├── docs/                      # Design documentation
│   ├── architecture.md        # This file
│   ├── staging.md             # Staging, travel, and VFS/ioctl behavior
│   ├── permissions.md         # Permission gating layer
│   └── cli.md                 # CLI reference
├── kmod/                      # Kernel module
│   ├── Kbuild
│   ├── yolofs.h               # Internal state + user/kernel ioctl ABI
│   ├── super.c
│   ├── inode.c
│   ├── file.c
│   ├── dir.c
│   ├── dentry.c
│   ├── lookup.c
│   ├── staging.c
│   ├── journal.c
│   ├── perm.c
│   └── ioctl.c
├── Cargo.toml
├── Cargo.lock
├── user/                      # Userspace CLI source (Rust)
│   ├── main.rs
│   ├── lib.rs
│   ├── config.rs              # yolofs.toml management (read, rules, mount options)
│   ├── changeset.rs           # net-change model behind `yolo review` (cmd/review.rs renders it)
│   ├── perm.rs                # permission rule modes + allow/deny ask decisions
│   ├── cmd/                   # CLI subcommand implementations
│   │   ├── init.rs            # `yolo init` -- scaffold yolofs.toml + agent hooks
│   │   ├── abort.rs
│   │   ├── journal.rs         # `yolo journal` (raw record display, `-- <path>` filter)
│   │   ├── snapshot.rs      # `yolo snapshot` (create only)
│   │   ├── commit.rs
│   │   ├── review.rs          # `yolo review` (summary, or git-style diff with `--diff`)
│   │   ├── exec.rs
│   │   ├── load.rs            # `yolo load/unload/reload` -- kernel module management
│   │   ├── mount.rs           # mount, unmount, remount (auto-loads kmod, prompts on staged changes)
│   │   ├── travel.rs         # `yolo travel` -- travel to a previous snapshot
│   │   ├── timeline.rs        # `yolo timeline` command (snapshot/travel DAG)
│   │   └── watch.rs           # permission prompt daemon (handles TTY ownership)
│   ├── journal/               # journal parsing, timeline, and resolution
│   │   ├── types.rs           # Action, Marker, Record, Segment, Target, Op, Note
│   │   ├── parse.rs           # parse()  (pub(super))
│   │   ├── marker.rs          # MarkerIndex (find_marker + marker_at + segment_range + alive_segments)
│   │   ├── core.rs            # Journal (struct + new + read + live_segments_*)
│   │   ├── tree.rs            # DirTree, DirNode
│   │   └── plan.rs            # DirTree::into_plan() -- commit mutation planner
│   ├── ioctl.rs               # binary protocol structs + ioctl helpers
│   ├── kmsg.rs                # kernel log reading via /dev/kmsg
│   └── utils.rs               # shared helpers (session_dir, plural)
└── tests/                     # Integration tests
```
