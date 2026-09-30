# YoloFS [![CI](https://github.com/YoloFS/YoloFS/actions/workflows/ci.yml/badge.svg)](https://github.com/YoloFS/YoloFS/actions/workflows/ci.yml)

**Don't let AI agents YOLO your files.**
*Information and control in agent-native filesystems* (SOSP 2026)

[Website](https://yolofs.github.io/) · [Paper](https://arxiv.org/abs/2604.13536) ·
[Slides](https://yolofs.github.io/slides.pdf) · [Poster](https://yolofs.github.io/poster/)

![YoloFS demo: without YoloFS, an agent runs a malicious setup script and never notices; with YoloFS, it sees the change to ~/.bashrc, travels back to undo it, and the SSH key read asks the user first](https://yolofs.github.io/demo/demo.gif)

## The problem

AI coding agents run shell commands on your machine with your privileges.
You can let the agent run everything ("YOLO mode") and hope it never runs
`rm -rf foo ~/`, or approve every command, which stalls the agent until you
stop reading the prompts. Either way, an approval prompt shows you the
command, not what it does to your files.

We studied **290 public reports** of agents misusing files. **40%** of the
harm was unrecoverable, and in **68%** of cases the agent never noticed.
The causes point to two gaps:

- **Information gap** — neither users nor agents can tell what a command
  actually does to files. Guardrails check command strings, not effects:
  blocking `rm` doesn't stop `python -c "shutil.rmtree(...)"`.
- **Control gap** — harm can't be reliably prevented, or undone afterwards.

## Agent-native filesystems

The filesystem sees every access, no matter which command or tool makes it.
So we move information and control from the agent into the filesystem, with
three primitives:

<p align="center"><img src="https://yolofs.github.io/fig/shift.svg" width="640" alt="Traditional vs agent-native filesystems: in agent-native filesystems, the filesystem gives users and agents information and control"></p>

1. **Introspect effects** — show which files each command actually read and changed.
2. **Undo mutations** — let the agent try a command, inspect the result, and roll it back.
3. **Gate accesses** — stop things that can't be undone, like reading a secret,
   before they happen.

The agent can then work on its own. You step in only for sensitive accesses
and the final review.

## YoloFS

YoloFS is a Linux kernel filesystem plus a `yolo` CLI. It stacks on any local
filesystem (ext4, xfs, btrfs, …), becomes the root filesystem for the agent's
commands, and plugs into Claude Code, Copilot, and Gemini through their tool
hooks.

<p align="center"><img src="https://yolofs.github.io/fig/arch.svg" width="560" alt="YoloFS architecture: the agent and user talk to the yolo CLI; commands run on the YoloFS kernel filesystem, which is layered over the base filesystem"></p>

- 📝 **Staging** — every change lands in a staging area, not your files. You
  review it and commit or abort.
- 📸 **Snapshots & travel** — a snapshot after every command shows what it
  changed; travel goes back to any snapshot.
- 🔐 **Progressive permission** — path rules `allow`, `deny`, or `ask`. An
  `ask` pauses the access until you answer, so you refine the policy as you go
  instead of writing it all up front.

## Results

- **Safety** — on 11 routine tasks with hidden destructive side effects, Claude
  Code with YoloFS noticed and undid the damage on its own in **8 of 11**; the
  other 3 stayed staged for the user to reject. No agent without YoloFS
  reliably prevented the damage.
- **Autonomy** — on 112 routine tasks, user interactions dropped from 0.9 to
  **0.4 per task** for Claude Code, at 99% success.
- **Performance** — matches ext4 on file I/O and on a Linux kernel development
  workload (plus 3.5 s to commit over 100,000 files), where OverlayFS is 18%
  slower ([dashboard](https://yolofs.github.io/perf-results/report/)).

## Quick start

YoloFS needs Linux (kernel 6.8–7.x) and `sudo` to install its kernel module.
To keep it off your own machine, use [the VM](#trying-it-in-a-vm).

```bash
git clone https://github.com/YoloFS/YoloFS && cd YoloFS
./setup.sh                       # install build deps (Ubuntu/Debian); then open a new shell
make install                     # build + install the CLI and kernel module

cd /path/to/project
yolo init                        # write yolofs.toml, agent hooks, and agent guides
yolo mount                       # start the session
yolo watch                       # (another terminal) answer `ask` prompts
yolo run -- make build           # run a command; its changes are staged and shown
yolo review --diff               # inspect everything staged
yolo commit                      # apply to your real files (or `yolo abort`)
```

`yolo run` executes the command in an isolated view of the filesystem
(private pid and mount namespace) and snapshots afterwards. Nothing reaches
your real files until `yolo commit`.

## Usage

### Agent integration

`yolo init` sets up Claude Code, Gemini CLI, and Copilot (`--agents <name>...`
to pick):

- A pre-tool-use hook (`.claude/`, `.gemini/`, `.github/hooks/`) routes every
  shell command the agent runs through `yolo run`.
- An always-loaded guide (`CLAUDE.md`, `GEMINI.md`, `AGENTS.md`) tells the
  agent its writes are staged and how to inspect and rewind them.
- The agent may use only `review`, `audit`, `timeline`, `travel`, and
  `snapshot`. Committing is left to you.

### Permission rules

Rules map paths to access levels and apply to everything below them. Paths
without a rule default to `ask`.

| Level       | Read | Write |
|-------------|------|-------|
| `allow`     | ✓    | ✓     |
| `write-ask` | ✓    | ask   |
| `read-only` | ✓    | ✗     |
| `ask`       | ask  | ask   |
| `deny`      | ✗    | ✗ (a denied dir also can't be listed) |

```bash
yolo rule deny ~/.ssh            # the verb names the level
yolo rule write-ask /etc
yolo rule list                   # configured rules
yolo rule resolve src            # effective level for a path, and which rule sets it
```

`yolo watch` answers `ask` prompts one access at a time; use `yolo rule` to
make an answer stick. An unanswered ask is denied after `prompt_timeout`.

### Snapshots and travel

```bash
yolo timeline                    # snapshots and travels so far
yolo review 2..4                 # changes between two snapshots
yolo travel 2                    # go back to snapshot 2
yolo snapshot "before refactor"  # take one explicitly
yolo audit -- /src/main.rs       # every recorded operation on one file
```

You can travel to any snapshot, including ones on a branch you traveled away
from, so nothing is lost by undoing.

### Configuration

`yolo init` writes a commented `yolofs.toml` (see the
[default](user/templates/yolofs.toml)):

```toml
permission     = true            # gate accesses through [rules]
staging        = true            # stage writes instead of applying them
auto_snapshot  = true            # snapshot after each `yolo run`
prompt_timeout = 30              # seconds before an unanswered ask is denied (0 = wait forever)

[rules]                          # absolute paths, or relative to the session root
"."    = "allow"
"/etc" = "write-ask"
```

## Development

```bash
make build                       # CLI (cargo) + kernel module
make test                        # unit + e2e tests
```

### Trying it in a VM

`./vm.py` runs an Ubuntu 24.04 QEMU VM (accelerated via KVM or HVF) with this
repo shared at the same path — useful if you'd rather not load a development
kernel module on your machine, or your kernel is outside the supported range:

```bash
./vm.py                          # boot the VM (downloads the image on first run) + SSH shell
./vm.py -- ./setup.sh            # install build deps in the guest (first time only)
./vm.py -- make install test     # run commands in the VM over SSH
./vm.py stop                     # shut down (`reset` recreates it from scratch)
```

### GitHub Codespaces

A Codespace is enough to browse the code, build the CLI, and run
`make test-unit`. It can't load the kernel module, so use the VM for anything
that mounts.

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
