# Architecture

AgFS stacks on top of any lower filesystem (ext4, xfs, NFS, ...) using VFS
interposition. It adds two orthogonal capabilities:

| Capability            | Summary |
| --------------------- | ------- |
| **Staging-commit**    | Every write goes to a staging layer. Changes are invisible to the lower FS until an explicit `commit`. An `abort` discards them instantly. |
| **Permission gating** | Every file starts in the `ask` state. A rule engine promotes matching paths to `allow`, `allow-rw`, `allow-ro`, `allow-rx`, or `deny`. When a thread touches an `ask` file, the thread is put to sleep; a userspace daemon receives the request and writes back a decision that wakes the thread. |

## Design Goals

- **In-kernel, zero-copy data path** — no FUSE overhead, no context switches
  for allowed operations.
- **Unprivileged mounting** via user namespaces (same as current AgFS).
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
 │                    AgFS                           │
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
 │  ioctl on any AgFS directory fd (.agfs/mnt)      │
 │    ← AGFS_IOC_GET_REQUEST:  dequeue perm request │
 │    → AGFS_IOC_PUT_RESPONSE: post decision        │
 │    → AGFS_IOC_RULE_ADD/REMOVE: manage rules      │
 │    → AGFS_IOC_RESTORE: reset staging (commit/abort) or restore to checkpoint │
 │    → AGFS_IOC_CHECKPOINT: create checkpoint           │
 └──────────────────────────────────────────────────┘
```

The two layers execute in order for every VFS operation:

1. **Perm Gating Layer** — resolves the effective permission for the file.
   Only applies to **regular files** — directories always pass through
   (controlled by standard Unix permissions on the lower FS).
   If `ask`, sleeps the calling thread. If `deny`, returns `-EACCES`.
   If `allow-*`, falls through.
2. **Staging Layer** — routes reads to the staged inode if the file has been
   modified, otherwise to the base. Ensures writes go to staged inodes.
   Uses per-directory-inode dirent hash tables for deletions and renames.

All I/O is ultimately delegated to the lower filesystem via `kiocb` swapping
and `vfs_*()` calls.

## Why Stackable VFS?

- **Portability**: Works on any underlying filesystem (ext4, xfs, NFS, tmpfs).
- **File-level granularity**: Permission gating operates on files and
  directories, which map naturally to inodes in a stackable FS.
- **Simplicity**: No need to manage block allocation, journaling, or
  filesystem metadata. The lower FS handles all of that.

## Comparison with OverlayFS

AgFS uses a fundamentally different staging model from OverlayFS.

**Staging vs live union**: OverlayFS is a live union filesystem — the upper
layer *is* the persistent state. There is no commit or abort. A renamed
file is copied up to upper with `RENAME_WHITEOUT` and stays there forever.
AgFS treats staging as a flat inode store with in-memory dirent tables that
are explicitly committed or discarded via the journal.

**Copy-up**: OverlayFS always does a full copy-up on first write, even for
truncating writes (`echo "x" > file` copies the entire file, then
truncates). AgFS detects `O_TRUNC` and creates an empty staged inode
directly — zero copy for the most common agent write pattern.

**Rename**: OverlayFS does a real `vfs_rename()` in the upper directory,
which requires copy-up. AgFS does zero-copy renames by adding dirents
(tombstone on old parent, link on new parent). Rename chains
resolve naturally through the dirent table.

**Lookup**: OverlayFS does two lookups per component (upper + lower) and
merges the results. AgFS checks the parent's dirent table first, then
falls back to base — one lookup.

**Permission model**: OverlayFS uses standard Unix permissions only. AgFS
adds the progressive gating layer (ask/allow/deny) with the ask protocol
for interactive approval.

**On-disk format**: OverlayFS requires filesystem support for whiteouts
(`RENAME_WHITEOUT`, ext4/xfs). AgFS uses a flat inode store + append-only
journal, working on any lower FS. The journal uses typed record tags
(`A`/`M`/`D`/`R`/`P` for mutations, `K`/`T` for checkpoints/restores) so
each record is self-describing. All renames — staged or redirect — emit a
single R or P record carrying both source and destination paths.

## Lifecycle Example

```
# 1. Full interactive workflow (mount -> watch + run -> diff -> commit/abort)
$ cd /home/user/project
$ agfs
   -> creates .agfs/, mounts / -> .agfs/mnt, applies rules from agfs.toml,
     starts background watch daemon for permission requests, chroots into
     .agfs/mnt, spawns $SHELL with cwd preserved as the caller's original CWD
   -> on shell exit: stops watch daemon, runs `agfs diff`, prompts user to
     commit, abort, or keep staged (user runs `agfs unmount` when done)

# 1b. Or use individual commands for more control:
$ agfs mount
$ agfs watch &           # start daemon in background
$ agfs exec -- make build
$ agfs diff
$ agfs commit

# 1c. Install rules via CLI from the session root (attaches perm directly
#     to dentries)
$ agfs rule add src allow-rw
$ agfs rule add /etc deny
$ agfs rule add /etc/hosts allow-ro

# 2. Agent writes to a file matching an allow-rw rule
$ echo "hello" > /src/main.rs
   -> kernel: agfs_lookup("src") -> explicit rule on dentry -> perm=ALLOW_RW
   -> kernel: agfs_lookup("main.rs") -> no rule on dentry (NONE)
              -> agfs_cache_perm() walks up: main.rs(NONE) -> src(ALLOW_RW)
              -> caches ALLOW_RW on main.rs inode
   -> kernel: agfs_open() -> cached_perm=ALLOW_RW, O_WRONLY -> pass
   -> kernel: agfs_write_iter() -> pass-through to staged inode

# 3. Agent reads /etc/passwd (denied -- /etc has deny rule)
$ cat /etc/passwd
   -> kernel: agfs_lookup("etc") -> explicit rule on dentry -> perm=DENY
   -> kernel: agfs_lookup("passwd") -> no rule on dentry (NONE)
              -> agfs_cache_perm() walks up: passwd(NONE) -> etc(DENY)
              -> caches DENY on passwd inode
   -> kernel: agfs_open("passwd") -> cached_perm=DENY -> -EACCES

# 4. Agent reads /etc/hosts (explicit override -> allow-ro)
$ cat /etc/hosts
   -> kernel: agfs_lookup("hosts") -> explicit rule on dentry -> perm=ALLOW_RO
              -> agfs_cache_perm() -> caches ALLOW_RO on hosts inode
   -> kernel: agfs_open() -> cached_perm=ALLOW_RO -> pass

# 5. Agent reads /tmp/secrets (no rule anywhere -> walk up reaches root -> ask)
$ cat /tmp/secrets
   -> kernel: agfs_lookup("tmp") -> no rule on dentry (NONE)
   -> kernel: agfs_lookup("secrets") -> no rule on dentry (NONE)
              -> agfs_cache_perm() walks up: secrets(NONE) -> tmp(NONE) -> root(ASK)
              -> caches ASK on secrets inode
   -> kernel: agfs_open() -> cached_perm=ASK
   -> kernel: enqueue request, thread sleeps
   -> daemon: ioctl(GET_REQUEST) -> agfs_ctl_request { id:1, path:"/tmp/secrets", ... }
   -> daemon: decision: allow-ro
   -> daemon: ioctl(PUT_RESPONSE, agfs_ctl_response { id:1, decision:ALLOW_RO })
   -> kernel: wake thread, apply one-shot ALLOW_RO to this open
   -> kernel: open base/tmp/secrets read-only, proceed

# 6. Agent tries to write /etc/hosts (walk up finds ALLOW_RO)
$ echo x >> /etc/hosts
   -> kernel: agfs_open() -> ALLOW_RO, O_WRONLY -> -EACCES

# 7. Commit all staged changes to the real filesystem (userspace)
$ agfs commit
   -> userspace: replay journal -- apply renames, deletes, move inodes to base
   -> userspace: ioctl(AGFS_IOC_RESTORE) with tree_len=0 on .agfs/mnt
   -> kernel: release dirents, invalidate dentry + inode caches
   -> umount .agfs/mnt

# 8. Restore to a previous checkpoint (appends T record, no truncation)
$ agfs restore "after make build"
   -> CLI: Journal → find_checkpoint → live_segments_at_name → build tree → serialize tree
   -> CLI: ioctl(AGFS_IOC_RESTORE, { target_gen=2, tree_buf })
   -> kernel: wipe dirents, inject dirents from tree, increment gen to 4,
      append T record to journal
   -> journal is append-only — dead records remain but are filtered
      by Journal reachability on subsequent operations
```

## Source File Layout

```
agfs/
├── README.md
├── docs/                      # Design documentation
│   ├── architecture.md        # This file
│   ├── staging.md             # Staging-commit mechanism
│   ├── permissions.md         # Permission gating layer
│   ├── internals.md           # VFS ops, ioctl behavior & concurrency
│   └── cli.md                 # CLI reference
├── kmod/                      # Kernel module
│   ├── Kbuild
│   ├── agfs.h
│   ├── super.c
│   ├── inode.c
│   ├── file.c
│   ├── dentry.c
│   ├── lookup.c
│   ├── staging.c
│   ├── journal.c
│   ├── perm.c
│   └── ioctl.c
├── Cargo.toml
├── Cargo.lock
├── cli/                       # Userspace CLI source (Rust)
│   ├── main.rs
│   ├── lib.rs
│   ├── config.rs              # agfs.toml management (init, rules, mount options)
│   ├── cmd/                   # CLI subcommand implementations
│   │   ├── abort.rs
│   │   ├── audit.rs           # `agfs audit` command (raw record display, --path filter)
│   │   ├── checkpoint.rs      # `agfs checkpoint` (create only)
│   │   ├── commit.rs
│   │   ├── diff.rs            # `agfs status` + `agfs diff` (summary and verbose views)
│   │   ├── exec.rs
│   │   ├── load.rs            # `agfs load/unload/reload` -- kernel module management
│   │   ├── mount.rs           # mount, unmount, remount (auto-loads kmod, prompts on staged changes)
│   │   ├── restore.rs         # `agfs restore` -- restore to a previous checkpoint
│   │   ├── timeline.rs        # `agfs timeline` command (checkpoint/restore DAG)
│   │   └── watch.rs           # permission prompt daemon (handles TTY ownership)
│   ├── journal/               # journal parsing, timeline, and resolution
│   │   ├── types.rs           # Action, Marker, Record, DType, Segment, INO_REDIRECT
│   │   ├── parse.rs           # parse()  (pub(super))
│   │   ├── markers.rs         # Markers (lookup + range + alive_segments + checkpoint_at)
│   │   ├── journal.rs         # Journal (struct + new + read + live_segments_*)
│   │   └── tree.rs            # DirTree, Dirent, DirNode
│   ├── ioctl.rs               # binary protocol structs + ioctl helpers
│   ├── kmsg.rs                # kernel log reading via /dev/kmsg
│   └── utils.rs               # shared helpers (session_dir, plural)
└── tests/                     # Integration tests
```
