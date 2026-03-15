#!/usr/bin/env python3
"""Launch an Ubuntu 24.04 cloud image VM using QEMU."""

import argparse
import json
import os
import subprocess
import sys
import textwrap
import time
from pathlib import Path

DATA_DIR = Path(__file__).resolve().parent / "data" / "vm"
IMAGE_NAME = "ubuntu-24.04-minimal-cloudimg-amd64.img"
IMAGE_URL = f"https://cloud-images.ubuntu.com/minimal/releases/noble/release/{IMAGE_NAME}"
DISK_NAME = "disk.qcow2"
SEED_NAME = "seed.iso"
LOG_NAME = "vm.log"
DEFAULT_DISK_SIZE = "20G"
DEFAULT_RAM = "8G"
DEFAULT_CPUS = os.cpu_count()
DEFAULT_SSH_PORT = 2222
DEFAULT_USER = "ubuntu"
DEFAULT_PASSWORD = "ubuntu"
# Build artifact directories (gitignored) to symlink to VM-local storage
# so they don't write through to the host via 9p.
LOCAL_DIRS = ["kmod/build", "target"]


def download_image():
    image_path = DATA_DIR / IMAGE_NAME
    if image_path.exists():
        print(f"Base image already exists: {image_path}")
        return image_path
    print(f"Downloading Ubuntu 24.04 cloud image...")
    subprocess.run(
        ["wget", "-q", "--show-progress", "-O", str(image_path), IMAGE_URL],
        check=True,
    )
    print(f"Downloaded to {image_path}")
    return image_path


def _detect_image_format(image_path: Path) -> str:
    result = subprocess.run(
        ["qemu-img", "info", "--output=json", str(image_path)],
        check=True, capture_output=True, text=True,
    )
    return json.loads(result.stdout)["format"]


def create_disk(image_path: Path, disk_size: str, force: bool = False):
    disk_path = DATA_DIR / DISK_NAME
    if disk_path.exists() and not force:
        print(f"Disk image already exists: {disk_path}")
        return disk_path
    backing_fmt = _detect_image_format(image_path)
    print(f"Creating disk image ({disk_size}, backing format: {backing_fmt})...")
    subprocess.run(
        [
            "qemu-img",
            "create",
            "-f",
            "qcow2",
            "-b",
            str(image_path),
            "-F",
            backing_fmt,
            str(disk_path),
            disk_size,
        ],
        check=True,
    )
    return disk_path


def _read_ssh_pubkeys():
    """Read all public SSH keys from ~/.ssh/."""
    ssh_dir = Path.home() / ".ssh"
    keys = []
    for p in sorted(ssh_dir.glob("id_*.pub")):
        key = p.read_text().strip()
        if key:
            keys.append(key)
    return keys


def create_seed_iso(host_cwd: Path, force: bool = False):
    seed_path = DATA_DIR / SEED_NAME
    if seed_path.exists() and not force:
        print(f"Seed ISO already exists: {seed_path}")
        return seed_path

    pubkeys = _read_ssh_pubkeys()
    if pubkeys:
        keys_lines = "\n".join(f"        - {k}" for k in pubkeys)
        keys_yaml = f"ssh_authorized_keys:\n{keys_lines}"
    else:
        keys_yaml = "ssh_authorized_keys: []"

    # Build symlink commands for artifact directories
    vm_workspace = str(host_cwd)
    vm_local = f"{vm_workspace}-local"
    symlink_cmds = []
    for d in LOCAL_DIRS:
        local_dir = f"{vm_local}/{d}"
        mount_dir = f"{vm_workspace}/{d}"
        symlink_cmds.append(f"mkdir -p {local_dir}")
        symlink_cmds.append(f"rm -rf {mount_dir}")
        symlink_cmds.append(f"ln -s {local_dir} {mount_dir}")

    mount_cmds = [
        f"mkdir -p {vm_workspace}",
        f"mount -t 9p -o trans=virtio,version=9p2000.L,rw hostcwd {vm_workspace}",
        *symlink_cmds,
        f"chown -R {DEFAULT_USER}:{DEFAULT_USER} {vm_local}",
        f'echo "cd {vm_workspace}" >> /home/{DEFAULT_USER}/.bashrc',
    ]
    runcmd_yaml = "runcmd:\n" + "\n".join(f"  - {cmd}" for cmd in mount_cmds)

    user_data = (
        "#cloud-config\n"
        "hostname: ubuntu-vm\n"
        "manage_etc_hosts: true\n"
        "users:\n"
        f"  - name: {DEFAULT_USER}\n"
        "    sudo: ALL=(ALL) NOPASSWD:ALL\n"
        "    shell: /bin/bash\n"
        "    lock_passwd: false\n"
        f'    plain_text_passwd: "{DEFAULT_PASSWORD}"\n'
        f"    {keys_yaml}\n"
        "ssh_pwauth: true\n"
        f"{runcmd_yaml}\n"
    )

    meta_data = textwrap.dedent(f"""\
        instance-id: ubuntu-vm-001
        local-hostname: ubuntu-vm
    """)

    user_data_path = DATA_DIR / "user-data"
    meta_data_path = DATA_DIR / "meta-data"
    user_data_path.write_text(user_data)
    meta_data_path.write_text(meta_data)

    print("Creating cloud-init seed ISO...")
    subprocess.run(
        [
            "cloud-localds",
            str(seed_path),
            str(user_data_path),
            str(meta_data_path),
        ],
        check=True,
    )
    return seed_path


def _ssh_cmd() -> list[str]:
    """Base SSH command with common options."""
    return [
        "ssh",
        "-o", "StrictHostKeyChecking=no",
        "-o", "UserKnownHostsFile=/dev/null",
        "-o", "LogLevel=ERROR",
        "-o", "PasswordAuthentication=no",
        "-o", "BatchMode=yes",
        "-p", str(DEFAULT_SSH_PORT),
        f"{DEFAULT_USER}@localhost",
    ]


def _print_vm_info():
    """Print VM connection info."""
    log_path = DATA_DIR / LOG_NAME
    print(f"  SSH:  ssh -p {DEFAULT_SSH_PORT} {DEFAULT_USER}@localhost")
    print(f"  Log:  {log_path}")
    print(f"  Stop: ./vm.py stop")


def is_vm_running():
    """Check if the VM is running via pidfile."""
    pidfile = DATA_DIR / "vm.pid"
    if not pidfile.exists():
        return False
    pid = int(pidfile.read_text().strip())
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        pidfile.unlink(missing_ok=True)
        return False


def wait_for_vm(mount_path: str, timeout: int = 120):
    """Block until SSH is reachable and the 9p mount is available."""
    start = time.time()
    while time.time() - start < timeout:
        result = subprocess.run(
            _ssh_cmd() + ["-o", "ConnectTimeout=2",
                          f"mountpoint -q {mount_path}"],
            capture_output=True,
        )
        if result.returncode == 0:
            return
        time.sleep(2)
    print(f"Error: VM not ready after {timeout}s. Check log: {DATA_DIR / LOG_NAME}")
    sys.exit(1)


def ensure_vm_started():
    """Start the VM with defaults if not already running, and wait until ready."""
    if is_vm_running():
        print("VM already running.")
        _print_vm_info()
        return
    print("VM not running, starting...")
    image_path = download_image()
    disk_path = create_disk(image_path, DEFAULT_DISK_SIZE)
    seed_path = create_seed_iso(Path.cwd())
    run_vm(disk_path, seed_path, DEFAULT_RAM, DEFAULT_CPUS, [])
    wait_for_vm(str(Path.cwd()))


def run_vm(
    disk_path: Path,
    seed_path: Path,
    ram: str,
    cpus: int,
    extra_args: list[str],
    foreground: bool = False,
):
    pidfile = DATA_DIR / "vm.pid"
    log_path = DATA_DIR / LOG_NAME
    cmd = [
        "qemu-system-x86_64",
        "-enable-kvm",
        "-machine", "q35",
        "-cpu", "host",
        "-m", ram,
        "-smp", str(cpus),
        "-drive", f"file={disk_path},format=qcow2,if=virtio",
        "-drive", f"file={seed_path},format=raw,if=virtio",
        "-net", "nic,model=virtio",
        "-net", f"user,hostfwd=tcp::{DEFAULT_SSH_PORT}-:22",
        "-virtfs", f"local,path={Path.cwd()},mount_tag=hostcwd,security_model=none",
    ]

    if foreground:
        cmd += ["-nographic"]
        cmd += extra_args
        print(f"Starting VM (ssh: ssh -p {DEFAULT_SSH_PORT} {DEFAULT_USER}@localhost)...")
        print(f"  RAM={ram}  CPUs={cpus}")
        print("Press Ctrl-A X to exit QEMU console.\n")
        os.execvp(cmd[0], cmd)

    cmd += [
        "-display", "none",
        "-serial", f"file:{log_path}",
        "-monitor", "none",
        "-daemonize",
        "-pidfile", str(pidfile),
    ]
    cmd += extra_args
    print(f"Starting VM in background...")
    subprocess.run(cmd, check=True)
    _print_vm_info()


def stop_vm():
    pidfile = DATA_DIR / "vm.pid"
    if not pidfile.exists():
        print("No pidfile found; VM may not be running.")
        return
    pid = int(pidfile.read_text().strip())
    print(f"Stopping VM (pid {pid})...")
    try:
        os.kill(pid, 15)
        # Wait for the process to exit so a subsequent start doesn't race.
        while True:
            time.sleep(0.5)
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                break
    except ProcessLookupError:
        print("Process not found; cleaning up pidfile.")
    pidfile.unlink(missing_ok=True)


def ssh_vm(ssh_args: list[str]):
    host_cwd = str(Path.cwd())
    cmd = _ssh_cmd()
    if ssh_args:
        cmd += [f"cd {host_cwd} && source ~/.profile 2>/dev/null &&"] + ssh_args
    else:
        cmd += ["-t", f"cd {host_cwd} && exec $SHELL -l"]
    os.execvp(cmd[0], cmd)


def main():
    # ./vm.py          → auto-start VM + interactive shell
    # ./vm.py -- <cmd> → auto-start VM + run command via SSH
    if len(sys.argv) == 1 or sys.argv[1] == "--":
        cmd = sys.argv[2:] if len(sys.argv) > 1 else []
        DATA_DIR.mkdir(parents=True, exist_ok=True)
        ensure_vm_started()
        ssh_vm(cmd)
        return

    parser = argparse.ArgumentParser(description="Manage an Ubuntu 24.04 QEMU VM")
    sub = parser.add_subparsers(dest="command", required=True)

    def _add_vm_args(p):
        p.add_argument("--ram", default=DEFAULT_RAM, help=f"RAM (default: {DEFAULT_RAM})")
        p.add_argument("--cpus", type=int, default=DEFAULT_CPUS, help=f"CPUs (default: {DEFAULT_CPUS})")
        p.add_argument("--disk-size", default=DEFAULT_DISK_SIZE, help=f"Disk size (default: {DEFAULT_DISK_SIZE})")
        p.add_argument("--force", action="store_true", help="Recreate disk and seed images")
        p.add_argument("--foreground", "-f", action="store_true", help="Run VM in foreground (interactive)")
        p.add_argument("extra", nargs="*", help="Extra QEMU arguments")

    # --- start ---
    p_start = sub.add_parser("start", help="Start the VM (daemonized by default)")
    _add_vm_args(p_start)

    # --- stop ---
    sub.add_parser("stop", help="Stop a daemonized VM")

    # --- restart ---
    p_restart = sub.add_parser("restart", help="Stop and re-start the VM")
    _add_vm_args(p_restart)

    # --- download ---
    sub.add_parser("download", help="Download the base cloud image only")

    args = parser.parse_args()

    DATA_DIR.mkdir(parents=True, exist_ok=True)

    if args.command == "download":
        download_image()
    elif args.command == "stop":
        stop_vm()
    elif args.command in ("start", "restart"):
        if args.command == "restart":
            stop_vm()
        elif is_vm_running():
            print(f"VM is already running.")
            _print_vm_info()
            sys.exit(1)
        image_path = download_image()
        disk_path = create_disk(image_path, args.disk_size, force=args.force)
        seed_path = create_seed_iso(Path.cwd(), force=args.force)
        run_vm(disk_path, seed_path, args.ram, args.cpus, args.extra, foreground=args.foreground)


if __name__ == "__main__":
    main()
