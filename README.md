# YoloFS [![CI](https://github.com/YoloFS/YoloFS/actions/workflows/ci.yml/badge.svg)](https://github.com/YoloFS/YoloFS/actions/workflows/ci.yml)

**Don't let AI agents YOLO your files.**
*Information and control in agent-native filesystems* (SOSP 2026)

[Website](https://yolofs.github.io/) · [Paper](https://arxiv.org/abs/2604.13536) ·
[Slides](https://yolofs.github.io/slides.pdf) · [Poster](https://yolofs.github.io/poster/)

![YoloFS demo: an agent runs a malicious script; YoloFS asks before it reads the SSH key, shows the change it made to ~/.bashrc, and travels back to undo it](https://yolofs.github.io/demo.gif)

## The problem

AI coding agents run shell commands on your machine with your privileges.
Today you have two choices. Let the agent run everything ("YOLO mode") and
hope it never runs `rm -rf foo ~/`. Or approve every command, which stalls
the agent until you stop reading the prompts.

Approving a command also tells you very little. If the agent asks to run
`cargo build`, you'll say yes, but a dependency's build script can read your
SSH key and edit your shell config, and neither the prompt nor the command
says so. The agent doesn't know either.

We studied **290 public reports** of agents misusing files (Claude Code,
Codex, Copilot, Cursor, Gemini, …). Of the 207 incidents with known impact,
44% overwrote data, 39% deleted files, and 17% leaked secrets. 42% of the
harm was outside the project, **40% was unrecoverable**, and in 68% of cases
the agent never noticed.

The causes point to two gaps:

- **Information gap** — neither users nor agents can tell what a command
  actually does to files. Harness guardrails check command strings, not
  effects: blocking `rm` doesn't stop `python -c "shutil.rmtree(...)"`.
- **Control gap** — harm can't be reliably prevented, or undone afterwards.
  Policies are fixed up front, and sandboxes are too strict for real work, so
  users turn them off.

## Agent-native filesystems

The filesystem sees every access, no matter which command or tool makes it.
So we move information and control from the agent into the filesystem, with
three primitives:

<p align="center"><img src="https://yolofs.github.io/fig/shift.svg" width="640" alt="Traditional vs agent-native filesystems: in agent-native filesystems, the filesystem gives users and agents information and control"></p>

1. **Introspect effects** — show which files each command actually read and changed.
2. **Undo mutations** — let the agent try a command, inspect the result, and roll it back.
3. **Gate accesses** — stop things that can't be undone, like reading a secret,
   before they happen. Rules apply to paths, not commands.

The agent can then work on its own. You step in only for sensitive accesses
and the final review.

## YoloFS

YoloFS is a Linux kernel module plus a `yolo` CLI. It stacks on any local
filesystem (ext4, xfs, btrfs, …) with a zero-copy data path, becomes the root
filesystem for the agent's commands, and plugs into Claude Code, Copilot, and
Gemini through their tool hooks.

<p align="center"><img src="https://yolofs.github.io/fig/arch.svg" width="560" alt="YoloFS architecture: the agent and user talk to the yolo CLI; commands run on the YoloFS kernel filesystem, which is layered over the base filesystem"></p>

- 📝 **Staging** — every change goes to a staging area, not your files. You
  `yolo review`, then `yolo commit` or `yolo abort`. File contents and paths
  are decoupled, so renaming a large file is a pointer update, not a copy.
- 📸 **Snapshots & travel** — a snapshot after every command shows exactly
  what it changed, and `yolo travel` goes back. Snapshots are markers in a
  journal, so hundreds of them don't slow down normal file operations.
- 🔐 **Progressive permission** — path rules `allow`, `deny`, or `ask`. No
  complete policy is needed up front: an `ask` pauses the calling thread and
  shows you the real path and operation (e.g. read `~/.ssh/id_rsa`), and your
  answer can become a new rule.

## Results

- **Safety** — 11 routine tasks (lint, build, format, …) with damage hidden
  behind scripts, Makefiles, or binaries. No baseline agent reliably prevented
  it; with YoloFS, Claude Code noticed and undid the damage on its own in
  **8 of 11**, and the other 3 were still staged for the user to reject.
- **Autonomy** — on 112 single-file-operation tasks, Claude Code needed
  **0.4 user interactions per task** with YoloFS, down from 0.9 without it,
  at 99% success.
- **Performance** — file I/O matches ext4. On a Linux kernel development
  workload YoloFS matches ext4 (plus 3.5 s to commit over 100,000 files), while
  OverlayFS is 18% slower. See the
  [performance dashboard](https://yolofs.github.io/perf-results/report/).

## Quick start

YoloFS needs Linux (kernel 6.8–7.x) and `sudo` to install the kernel module.
To avoid loading it on your own machine, see [Trying it in a VM](#trying-it-in-a-vm).

```bash
git clone https://github.com/YoloFS/YoloFS && cd YoloFS
./setup.sh                       # install build deps (Ubuntu/Debian); then open a new shell
make install                     # build + install CLI and kernel module

cd /path/to/project
yolo init                        # scaffold yolofs.toml + agent hooks + agent guide
yolo mount                       # start the session
yolo watch                       # (another terminal) answer `ask` prompts as they arrive
yolo run -- make build           # stage the command's changes and show them
yolo review                      # inspect staged changes (`--diff` for the diff body)
yolo commit                      # apply to your real files, or `yolo abort` to discard
```

After `yolo init`, your coding agent's shell commands run through `yolo run`
automatically (see [Agent integration](#agent-integration)).

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

### Agent integration

`yolo init` scaffolds pre-tool-use hooks for Claude Code (`.claude/`), Gemini
CLI (`.gemini/`), and Copilot (`.github/hooks/`) — pass `--agents <name>...`
to pick — so every shell command the agent runs goes through `yolo run`
automatically. It also writes an always-loaded guide (`CLAUDE.md`,
`GEMINI.md`, or `AGENTS.md`) telling the agent its writes are staged and that
it may inspect and rewind (`review`, `audit`, `timeline`, `travel`,
`snapshot` — the navigation-only subcommands the CLI allows agents) but must
leave committing to you.

### Permission rules

Rules map paths to access levels and apply to everything below them:

| Level       | Read | Write |
|-------------|------|-------|
| `allow`     | ✓    | ✓     |
| `write-ask` | ✓    | ask   |
| `read-only` | ✓    | ✗     |
| `ask`       | ask  | ask   |
| `deny`      | ✗    | ✗ (a denied dir also can't be listed) |

```bash
yolo rule allow src          # the verb names the level
yolo rule write-ask /etc
yolo rule deny ~/.ssh
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

### Trying it in a VM

If you'd rather not load a development kernel module on your own machine —
or your kernel is outside the supported range — `./vm.py` manages a QEMU VM
(Ubuntu 24.04, hardware-accelerated via KVM or HVF) with this repo shared
into the guest at the same path:

```bash
./vm.py                          # boot the VM (downloads the image on first run) + SSH shell
./vm.py -- ./setup.sh            # install build deps in the guest (first time only)
./vm.py -- make install test     # run commands in the VM over SSH
./vm.py stop                     # shut the VM down (`reset` recreates it from scratch)
```

### Trying it in GitHub Codespaces

For a quick cloud-based setup, GitHub Codespaces also works well for basic
CLI and test iteration without managing a local VM or kernel setup.

[![Open in GitHub Codespaces](https://github.com/codespaces/badge.svg)](https://codespaces.new/YoloFS/YoloFS?quickstart=1)

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

## Citation

```bibtex
@inproceedings{yolofs-sosp26,
  title     = {Don't Let AI Agents YOLO Your Files: Information and Control
               in Agent-Native Filesystems},
  author    = {Zhong, Shawn Wanxiang and Liao, Junxuan and Liu, Jing and
               Zheng, Mai and Arpaci-Dusseau, Andrea C. and
               Arpaci-Dusseau, Remzi H.},
  booktitle = {Symposium on Operating Systems Principles (SOSP '26)},
  year      = {2026},
  doi       = {10.1145/3830418.3843858}
}
```
