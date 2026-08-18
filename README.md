# YoloFS

**Don't let AI agents YOLO your files.**

AI coding agents run hundreds of file operations and shell commands on your
machine, with your privileges. They have wiped drives, destroyed personal
documents, and silently leaked credentials — and the usual defense, a
command-approval prompt, shows you an innocuous-looking command with no
indication of its actual filesystem effects. Approval fatigue sets in, and
users end up in "YOLO mode," letting the agent run unchecked.

The root problem is two gaps: neither you nor the agent has **information**
about what a command did to the filesystem, and neither has **control** to
prevent or undo it. YoloFS closes both gaps by shifting information and
control from the agent to the filesystem itself.

YoloFS is an **agent-native filesystem**: a Linux kernel filesystem that
stacks on any local base filesystem (ext4, xfs, btrfs, …) via VFS interposition with
a zero-copy data path, plus a `yolo` CLI. It provides three mechanisms:

- **Staging** — every mutation is isolated in a staging area instead of being
  applied to your files. You review the accumulated changes and commit or
  abort them. Arbitrary path mutations are handled without copying unchanged
  data.
- **Snapshots & travel** — the agent (or you) can snapshot after each
  command, inspect exactly what changed, and travel back to any earlier
  state to undo mistakes and retry — without slowing normal file operations.
- **Progressive permission** — path-based rules `allow`, `deny`, or `ask`
  about accesses as they happen. No complete upfront policy needed: you
  refine rules interactively based on what the agent actually touches.

Together these let an agent work autonomously and correct its own mistakes,
while your interaction is reserved for sensitive accesses and final review.

**Example.** An agent runs `cargo build` on a project with a compromised
dependency whose build script reads your SSH key and edits your shell config.
Under YoloFS the command runs without a command-level prompt — but the SSH
key read trips an `ask` rule, so you see the actual path and operation and
deny it. All writes land in staging, not your real files. After the command,
the agent inspects the denied access and staged changes, recognizes the
attack, travels back to the previous snapshot, and retries with a different
dependency. At the end, you review what remains and commit.

See the [website](https://yolofs.github.io/) for an overview and the
[paper](https://arxiv.org/abs/2604.13536) for the misuse study, design, and
evaluation.

## Quick start

`yolo` is a host-side tool — like `docker`, you run it **outside** the mount
and it manages the session; every `yolo` command refuses to run inside the
mount.

```bash
make install                     # build + install CLI and kernel module

cd /path/to/project
yolo init                        # scaffold yolofs.toml + agent hooks + agent guide
yolo run -- make build           # mounts on first run, stages the command, shows changes
yolo review                      # inspect staged changes (`--diff` for the diff body)
yolo commit                      # apply to your real files, or `yolo abort` to discard
```

## Usage

### Session workflow

`yolo init` creates `yolofs.toml` and per-agent hook files. `yolo run -- <cmd>`
runs a command through YoloFS: it mounts on demand, executes the command in an
isolated view (private pid + mount namespace, pivoted onto the mount), stages
all of its writes, auto-snapshots, and prints a review summary. Nothing
touches your real files until you decide:

```bash
yolo review                  # summary of staged changes
yolo review --diff           # full diff
yolo commit                  # apply staged changes to the base filesystem
yolo abort                   # discard everything staged
```

### Permission rules

Rules map paths to access levels; the verb names the level:

```bash
yolo rule allow src          # free read/write
yolo rule write-ask /etc     # allow reads, ask before writes
yolo rule read-only /usr
yolo rule deny ~/.ssh        # no read/write; for a dir also blocks listing
yolo rule ask /etc/hosts     # force a prompt, overriding an inherited rule
yolo rule list               # configured rules
yolo rule resolve src        # effective level for a path + where it comes from
```

Run `yolo watch` (e.g. in another terminal) to answer `ask` prompts as they
arrive; each answer applies to that one access only — use `yolo rule` to
refine the policy for the rest of the session. An unanswered ask is denied
after `prompt_timeout`. Files under the session root
are typically ruled `allow`; everything else defaults to `ask`.

### Snapshots and travel

```bash
yolo snapshot "before refactor"  # explicit snapshot (auto after each `yolo run` that changed something)
yolo timeline                    # snapshot/travel DAG
yolo review 2..4                 # changes between two snapshots
yolo travel 2                    # restore the state at snapshot 2
yolo audit -- /src/main.rs       # journal records for one file
```

Any generation id is a valid travel target, so mistakes can be undone and
retried without losing earlier history.

### Agent integration

`yolo init` scaffolds pre-tool-use hooks for Claude Code (`.claude/`), Gemini
CLI (`.gemini/`), and Copilot (`.github/hooks/`) — pass `--agents <name>...`
to pick — so every shell command the agent runs goes through `yolo run`
automatically. It also writes an always-loaded guide (`CLAUDE.md`,
`GEMINI.md`, or `AGENTS.md`) telling the agent its writes are staged and that
it may inspect and rewind (`review`, `audit`, `timeline`, `travel`,
`snapshot` — the navigation-only subcommands the CLI allows agents) but must
leave committing to you.

### Configuration

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

## Building

**Prerequisites**: Linux kernel headers, Rust toolchain, `make` —
`./setup.sh` installs all of them on Ubuntu/Debian. Kernels 6.8 through 7.x
are what CI and the dev VM run.

```bash
make build                       # CLI (cargo) + kernel module
make install                     # install to /usr/local/bin and /lib/modules
make test                        # run unit + e2e tests
```

The binary is installed with file capabilities (`cap_sys_admin`,
`cap_sys_module`), not setuid root — it always runs as the invoking user.

### Trying it in a VM

If you'd rather not load a development kernel module on your own machine —
or your kernel is outside the supported range — `./vm.py` manages a QEMU VM
(Ubuntu 24.04, KVM-accelerated) with this repo shared into the guest at the
same path:

```bash
./vm.py                          # boot the VM (downloads the image on first run) + SSH shell
./vm.py -- ./setup.sh            # install build deps in the guest (first time only)
./vm.py -- make install test     # run commands in the VM over SSH
./vm.py stop                     # shut the VM down (`reset` recreates it from scratch)
```

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
