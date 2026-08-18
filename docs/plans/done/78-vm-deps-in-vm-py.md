# Move VM host dependencies from setup.sh into vm.py

## Problem

`setup.sh` installs the QEMU/xorriso packages (`qemu-system-x86`,
`qemu-utils`, `xorriso`) on every machine except the guest, guarded by a
hostname check (`hostname != ubuntu-vm`). This has three problems:

1. Hosts that never use the VM still get QEMU installed.
2. The hostname guard duplicates `VM_HOSTNAME` from vm.py and breaks if
   either side drifts or another machine is named `ubuntu-vm`.
3. `qemu-system-x86` is wrong on arm64 hosts, where vm.py launches
   `qemu-system-aarch64` (provided by `qemu-system-arm` on Ubuntu).

## Plan

1. **vm.py**: add `_ensure_deps()` that checks for the binaries vm.py
   actually invokes — the arch-appropriate `qemu-system-*` (per
   `GUEST_ARCH`), `qemu-img`, and `xorriso`. If any are missing:
   - on apt-based systems, print the exact command and run
     `sudo apt-get update` + `sudo apt-get install -y
     --no-install-recommends <missing packages>`;
   - otherwise (e.g. macOS), exit with an actionable hint
     (`brew install qemu xorriso`).

   Call it from `ensure_vm_started()` (the bare `./vm.py` path) and from
   the `start`/`restart`/`reset` subcommands. `stop`/`download` don't
   need QEMU. Remove the now-redundant standalone xorriso check in
   `_write_seed_iso()`.

2. **setup.sh**: delete `vm_deps` and the hostname conditional; keep only
   the build deps (needed on both host and guest).

3. **README**: no change needed — it already describes `setup.sh` as
   installing build prerequisites and `./vm.py` as self-managing.

## Non-goals

- No change to guest provisioning (`./vm.py -- ./setup.sh` still installs
  build deps inside the VM).
- No support for auto-installing on non-apt Linux; those get the hint.
