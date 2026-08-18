# YoloFS

A Linux kernel stackable filesystem for AI coding agents. Every write
goes to a staging area (commit when satisfied, abort to discard); every
file access is gated by a path-based rule engine that can allow, deny, or
interactively prompt. Stacks on any lower filesystem (ext4, xfs, NFS, …)
via VFS interposition with a zero-copy data path.

See the [website](https://yolofs.github.io/) for an overview and the
[paper](https://arxiv.org/abs/2604.13536) for details.

## Quick start

`yolo` is a host-side tool — like `docker`, you run it **outside** the mount and
it manages the session; every `yolo` command refuses to run inside the mount.

```bash
make install                     # build + install CLI and kernel module

cd /path/to/project
yolo init                        # initialize a session (yolofs.toml + agent hooks + agent guide)
yolo mount                       # mount the session
yolo watch &                     # permission prompt daemon
yolo run -- make build               # run a command in the staging overlay (shows changes)
yolo review                      # review staged changes (`--diff` for the diff body)
yolo commit                      # apply, or `yolo abort` to discard
```

## Building

**Prerequisites**: Linux kernel headers, Rust toolchain, `make`.

```bash
make build                       # CLI (cargo) + kernel module
make install                     # install to /usr/local/bin and /lib/modules
make test                        # run unit + e2e tests
```

## Configuration

`yolofs.toml` in the session directory:

```toml
permission     = true            # enable permission gating
staging        = true            # enable staging area
auto_snapshot  = true            # snapshot after each command run through yolofs
prompt_timeout = 30              # seconds to wait for an `ask` answer before denying (0 = infinite)

[rules]
"."          = "allow"
"/etc"       = "write-ask"
"/etc/hosts" = "read-only"
"/usr/bin"   = "read-only"
```

Paths in `[rules]` can be absolute or relative to the session root.

## Documentation

- [Architecture](docs/architecture.md) — high-level design, lifecycle, source layout
- [Staging](docs/staging.md) — COW, journal, path resolution, snapshots
- [Permissions](docs/permissions.md) — rule engine, ask protocol
- [CLI](docs/cli.md) — commands, options, terminal handling

## Related repositories

- [`perf-eval`](https://github.com/YoloFS/perf-eval) — performance benchmark suite (`yolo-bench`)
- [`perf-results`](https://github.com/YoloFS/perf-results) — benchmark output data
- [`agent-eval`](https://github.com/YoloFS/agent-eval) — agent behavior evaluation harness
- [`sosp-ae`](https://github.com/YoloFS/sosp-ae) — SOSP artifact evaluation instructions
- [`yolofs.github.io`](https://github.com/YoloFS/yolofs.github.io) — project website source
