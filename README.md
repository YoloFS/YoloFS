# YoloFS — Agentic Filesystem

A Linux kernel stackable filesystem that provides **staging-commit semantics**
and **progressive permission gating** for AI-agent sandboxing.

YoloFS stacks on top of any lower filesystem (ext4, xfs, NFS, ...) using VFS
interposition. Every write goes to a staging layer —
invisible to the lower FS until an explicit `commit`. Every file access is
gated by a rule engine that can allow, deny, or interactively prompt a human
before the agent proceeds.

## Features

| Capability            | Summary |
| --------------------- | ------- |
| **Staging-commit**    | Every write goes to a staging layer. Changes are invisible to the lower FS until an explicit `commit`. An `abort` discards them instantly. |
| **Permission gating** | Every file starts in the `ask` state. A rule engine promotes matching paths to `allow`, `allow-rw`, `allow-ro`, `allow-rx`, or `deny`. When a thread touches an `ask` file, the thread is put to sleep; a userspace daemon receives the request and writes back a decision that wakes the thread. |

Design goals:

- **In-kernel, zero-copy data path** — no FUSE overhead, no context switches
  for allowed operations.
- **Unprivileged mounting** via user namespaces.
- **Composable** — staging and permission gating are independent layers;
  either can be disabled at mount time.

## Architecture

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
```

## Quick Demo

```bash
# Build and install
make install

# Initialize a session in your project directory
cd /home/user/project
yolo init

# Configure rules (or edit yolofs.toml directly)
yolo rule add .         allow-rw    # project files: full access
yolo rule add /etc      deny        # system config: blocked
yolo rule add /etc/hosts allow-ro   # except hosts: read-only
yolo rule add /usr/bin  allow-rx    # binaries: run but not modify

# Launch an interactive sandbox (mount + watch + shell)
yolo
# ... agent or shell works inside the sandbox ...
# On exit: shows diff, prompts to commit/abort

# Or use individual commands for more control:
yolo mount
yolo watch &                # background daemon for permission prompts
yolo exec -- make build     # run a command inside the sandbox
yolo status                 # show staged changes
yolo diff                   # git-style diff of staged vs base
yolo commit                 # apply changes to the real filesystem
# yolo abort                # or discard everything
```

## How It Compares

**Staging** (vs OverlayFS):

| Aspect | YoloFS | OverlayFS |
|--------|------|-----------|
| **Model** | Explicit commit/abort with checkpoints | Live union — upper *is* the state |
| **Truncating write** | Zero-copy (empty inode) | Full copy-up, then truncate |
| **Rename** | Zero-copy via dirent metadata | `vfs_rename()` with copy-up |
| **Lookup** | Dirent table, then base — one lookup | Upper + lower — two lookups |
| **On-disk format** | Flat inode store + journal (any lower FS) | Requires whiteout support (ext4/xfs) |

**Permissions** (vs Landlock):

| Aspect | YoloFS | Landlock |
|--------|------|----------|
| **Default policy** | Ask (block + prompt) | Deny (handled rights) |
| **Overlapping rules** | Nearest-ancestor wins (both directions) | Additive only (can't deny child of allowed parent) |
| **Dynamic rules** | Add/remove/change at runtime | Immutable after enforce |
| **Access check cost** | O(1) in steady state via inode cache + generation counter | O(depth x log n) per ancestor |
| **Scope** | Per-mount | Per-process (cred-attached) |

## Building

**Prerequisites**: Linux kernel headers, Rust toolchain, `make`.

```bash
make build      # build CLI (cargo) + kernel module
make install    # install CLI to /usr/local/bin, kmod to /lib/modules
make test       # run unit + e2e tests
```

## Configuration

YoloFS is configured via `yolofs.toml` in the session directory:

```toml
permission = true       # enable permission gating
staging = true          # enable staging area
ask_default = "deny"    # fallback when no daemon or on timeout
ask_timeout = 30        # seconds before ask request times out (0 = infinite)
checkpoint = true         # auto-checkpoint after each `yolo exec`

[rules]
"."          = "allow-rw"
"/etc"       = "deny"
"/etc/hosts" = "allow-ro"
"/usr/bin"   = "allow-rx"
```

Paths in `[rules]` can be **absolute** (`/etc`) or **relative** to the session
root (the directory containing `.yolofs/`).

## Documentation

| Document | Description |
|----------|-------------|
| [Architecture](docs/architecture.md) | High-level design, lifecycle walkthrough, source layout |
| [Staging Layer](docs/staging.md) | Staging-commit mechanism: COW, journal, path resolution, checkpoints |
| [Permission Layer](docs/permissions.md) | Permission gating: rule engine, ask protocol, Landlock comparison |
| [Kernel Reference](docs/internals.md) | Data structures, VFS operations map, ioctl interface, concurrency |
| [CLI Reference](docs/cli.md) | Commands, options, terminal handling |
