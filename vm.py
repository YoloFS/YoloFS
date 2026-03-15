#!/usr/bin/env python3
"""Launch an Ubuntu 24.04 cloud image VM using QEMU."""

import argparse
import json
import os
import subprocess
import sys
import textwrap
from pathlib import Path

DATA_DIR = Path(__file__).resolve().parent / "data" / "vm"
IMAGE_NAME = "ubuntu-24.04-minimal-cloudimg-amd64.img"
IMAGE_URL = f"https://cloud-images.ubuntu.com/minimal/releases/noble/release/{IMAGE_NAME}"
DISK_NAME = "disk.qcow2"
SEED_NAME = "seed.iso"
DEFAULT_DISK_SIZE = "20G"
DEFAULT_RAM = "8G"
DEFAULT_CPUS = os.cpu_count()
DEFAULT_SSH_PORT = 2222
DEFAULT_USER = "ubuntu"
DEFAULT_PASSWORD = "ubuntu"


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


def create_seed_iso(user: str, password: str, force: bool = False):
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

    user_data = (
        "#cloud-config\n"
        "hostname: ubuntu-vm\n"
        "manage_etc_hosts: true\n"
        "users:\n"
        f"  - name: {user}\n"
        "    sudo: ALL=(ALL) NOPASSWD:ALL\n"
        "    shell: /bin/bash\n"
        "    lock_passwd: false\n"
        f'    plain_text_passwd: "{password}"\n'
        f"    {keys_yaml}\n"
        "ssh_pwauth: true\n"
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


def run_vm(
    disk_path: Path,
    seed_path: Path,
    ram: str,
    cpus: int,
    ssh_port: int,
    extra_args: list[str],
    daemonize: bool = False,
):
    pidfile = DATA_DIR / "vm.pid"
    if pidfile.exists():
        pid = int(pidfile.read_text().strip())
        try:
            os.kill(pid, 0)
            print(f"Error: VM is already running (pid {pid}).")
            print(f"Connect with: python3 {__file__} ssh")
            sys.exit(1)
        except ProcessLookupError:
            pidfile.unlink(missing_ok=True)

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
        "-net", f"user,hostfwd=tcp::{ssh_port}-:22",
        "-nographic",
    ]
    if daemonize:
        pidfile = DATA_DIR / "vm.pid"
        cmd += ["-daemonize", "-pidfile", str(pidfile)]
    cmd += extra_args

    print(f"Starting VM (ssh: ssh -p {ssh_port} {DEFAULT_USER}@localhost)...")
    print(f"  RAM={ram}  CPUs={cpus}")
    if not daemonize:
        print("Press Ctrl-A X to exit QEMU console.\n")
    os.execvp(cmd[0], cmd)


def stop_vm():
    pidfile = DATA_DIR / "vm.pid"
    if not pidfile.exists():
        print("No pidfile found; VM may not be running.")
        return
    pid = int(pidfile.read_text().strip())
    print(f"Stopping VM (pid {pid})...")
    try:
        os.kill(pid, 15)
    except ProcessLookupError:
        print("Process not found; cleaning up pidfile.")
    pidfile.unlink(missing_ok=True)


def ssh_vm(ssh_port: int, user: str, ssh_args: list[str]):
    cmd = [
        "ssh",
        "-o", "StrictHostKeyChecking=no",
        "-o", "UserKnownHostsFile=/dev/null",
        "-o", "PasswordAuthentication=no",
        "-o", "BatchMode=yes",
        "-p", str(ssh_port),
        f"{user}@localhost",
    ]
    if ssh_args:
        cmd += [f"source ~/.profile 2>/dev/null &&"] + ssh_args
    else:
        cmd += ["-t", "exec $SHELL -l"]
    os.execvp(cmd[0], cmd)


def main():
    parser = argparse.ArgumentParser(description="Manage an Ubuntu 24.04 QEMU VM")
    sub = parser.add_subparsers(dest="command", required=True)

    # --- start ---
    p_start = sub.add_parser("start", help="Start the VM")
    p_start.add_argument("--ram", default=DEFAULT_RAM, help=f"RAM (default: {DEFAULT_RAM})")
    p_start.add_argument("--cpus", type=int, default=DEFAULT_CPUS, help=f"CPUs (default: {DEFAULT_CPUS})")
    p_start.add_argument("--ssh-port", type=int, default=DEFAULT_SSH_PORT, help=f"Host SSH port (default: {DEFAULT_SSH_PORT})")
    p_start.add_argument("--disk-size", default=DEFAULT_DISK_SIZE, help=f"Disk size (default: {DEFAULT_DISK_SIZE})")
    p_start.add_argument("--user", default=DEFAULT_USER)
    p_start.add_argument("--password", default=DEFAULT_PASSWORD)
    p_start.add_argument("--force", action="store_true", help="Recreate disk and seed images")
    p_start.add_argument("--daemonize", "-d", action="store_true", help="Run VM in background")
    p_start.add_argument("extra", nargs="*", help="Extra QEMU arguments")

    # --- stop ---
    sub.add_parser("stop", help="Stop a daemonized VM")

    # --- ssh ---
    p_ssh = sub.add_parser("ssh", help="SSH into the VM")
    p_ssh.add_argument("--ssh-port", type=int, default=DEFAULT_SSH_PORT)
    p_ssh.add_argument("--user", default=DEFAULT_USER)
    p_ssh.add_argument("extra", nargs="*", help="Extra SSH arguments")

    # --- download ---
    sub.add_parser("download", help="Download the base cloud image only")

    args = parser.parse_args()

    DATA_DIR.mkdir(parents=True, exist_ok=True)

    if args.command == "download":
        download_image()
    elif args.command == "start":
        image_path = download_image()
        disk_path = create_disk(image_path, args.disk_size, force=args.force)
        seed_path = create_seed_iso(args.user, args.password, force=args.force)
        run_vm(disk_path, seed_path, args.ram, args.cpus, args.ssh_port, args.extra, args.daemonize)
    elif args.command == "stop":
        stop_vm()
    elif args.command == "ssh":
        ssh_vm(args.ssh_port, args.user, args.extra)


if __name__ == "__main__":
    main()
