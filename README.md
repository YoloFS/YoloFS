# YoloFS

A Linux kernel stackable filesystem for AI-agent sandboxing. Every write
goes to a staging area (commit when satisfied, abort to discard); every
file access is gated by a path-based rule engine that can allow, deny, or
interactively prompt. Stacks on any lower filesystem (ext4, xfs, NFS, …)
via VFS interposition with a zero-copy data path.

See the [website](https://yolofs.github.io/) for an overview and the
[paper](https://arxiv.org/abs/2604.13536) for details. Part of the
[YoloFS](https://github.com/YoloFS/YoloFS) project.

## Quick start

```bash
make install                     # build + install CLI and kernel module

cd /path/to/project
yolo init                        # initialize a session
yolo                             # interactive: mount + permission daemon + shell
                                 # on exit: review diff, commit or abort
```

Non-interactive control:

```bash
yolo mount
yolo watch &                     # permission prompt daemon
yolo exec -- make build          # run a command in the sandbox
yolo status                      # show staged changes
yolo diff                        # git-style diff vs base
yolo commit                      # apply, or `yolo abort` to discard
```

## Building

**Prerequisites**: Linux kernel headers, Rust toolchain, `make`.

```bash
make build                       # CLI (cargo) + kernel module
make install                     # install to /usr/local/bin and /lib/modules
make vm-test                     # run unit + e2e tests in a VM (recommended)
```

## Configuration

`yolofs.toml` in the session directory:

```toml
permission   = true              # enable permission gating
staging      = true              # enable staging area
ask_default  = "deny"            # fallback when no daemon or on timeout
ask_timeout  = 30                # ask timeout (seconds; 0 = infinite)
checkpoint   = true              # auto-checkpoint after each `yolo exec`

[rules]
"."          = "allow"
"/etc"       = "deny"
"/etc/hosts" = "ro"
"/usr/bin"   = "ro"
```

Paths in `[rules]` can be absolute or relative to the session root.

## Documentation

- [Architecture](docs/architecture.md) — high-level design, lifecycle, source layout
- [Staging](docs/staging.md) — COW, journal, path resolution, checkpoints
- [Permissions](docs/permissions.md) — rule engine, ask protocol
- [Internals](docs/internals.md) — data structures, VFS map, ioctls, concurrency
- [CLI](docs/cli.md) — commands, options, terminal handling
