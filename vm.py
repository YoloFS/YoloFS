#!/usr/bin/env python3
"""Launch an Ubuntu 24.04 cloud image VM using QEMU."""

import argparse
import json
import os
import platform
import secrets
import shutil
import subprocess
import sys
import textwrap
import time
from pathlib import Path

GUEST_ARCH = "arm64" if platform.machine() in ("arm64", "aarch64") else "amd64"

REPO_DIR = Path(__file__).resolve().parent
DATA_DIR = REPO_DIR / "vm"
IMAGE_NAME = f"ubuntu-24.04-minimal-cloudimg-{GUEST_ARCH}.img"
IMAGE_PATH = DATA_DIR / IMAGE_NAME
IMAGE_URL = f"https://cloud-images.ubuntu.com/minimal/releases/noble/release/{IMAGE_NAME}"
DISK_PATH = DATA_DIR / "disk.qcow2"
SEED_PATH = DATA_DIR / "seed.iso"
LOG_PATH = DATA_DIR / "vm.log"
PID_PATH = DATA_DIR / "vm.pid"
SSH_KEY_PATH = DATA_DIR / "id_ed25519"
SSH_PUBKEY_PATH = DATA_DIR / "id_ed25519.pub"
SSH_KNOWN_HOSTS = DATA_DIR / "known_hosts"
SSH_CONFIG_PATH = DATA_DIR / "ssh_config"
DEFAULT_DISK_SIZE = "20G"
DEFAULT_RAM = "32G"
DEFAULT_CPUS = os.cpu_count()
DEFAULT_SSH_PORT = 2222
DEFAULT_USER = "ubuntu"
PASSWORD_PATH = DATA_DIR / "password"
VM_HOSTNAME = "ubuntu-vm"


def download_image():
    if IMAGE_PATH.exists():
        print(f"Base image already exists: {IMAGE_PATH}")
        return
    print(f"Downloading Ubuntu 24.04 cloud image...")
    subprocess.run(
        ["curl", "-fL", "--progress-bar", "-o", str(IMAGE_PATH), IMAGE_URL],
        check=True,
    )
    print(f"Downloaded to {IMAGE_PATH}")


def _detect_image_format() -> str:
    result = subprocess.run(
        ["qemu-img", "info", "--output=json", str(IMAGE_PATH)],
        check=True, capture_output=True, text=True,
    )
    return json.loads(result.stdout)["format"]


def create_disk(disk_size: str, reset: bool = False):
    if DISK_PATH.exists() and not reset:
        print(f"Disk image already exists: {DISK_PATH}")
        return
    backing_fmt = _detect_image_format()
    print(f"Creating disk image ({disk_size}, backing format: {backing_fmt})...")
    subprocess.run(
        [
            "qemu-img",
            "create",
            "-f",
            "qcow2",
            "-b",
            str(IMAGE_PATH),
            "-F",
            backing_fmt,
            str(DISK_PATH),
            disk_size,
        ],
        check=True,
    )


def _ensure_password() -> str:
    """Ensure a VM password file exists under DATA_DIR, return the password."""
    if not PASSWORD_PATH.exists():
        password = secrets.token_urlsafe(16)
        PASSWORD_PATH.write_text(password)
        PASSWORD_PATH.chmod(0o600)
    return PASSWORD_PATH.read_text().strip()


def _ensure_ssh_keypair() -> str:
    """Ensure a VM-specific SSH keypair exists under DATA_DIR, return the public key."""
    if not SSH_KEY_PATH.exists():
        print("Generating VM SSH keypair...")
        subprocess.run(
            ["ssh-keygen", "-t", "ed25519", "-f", str(SSH_KEY_PATH), "-N", "", "-C", "vm"],
            check=True, capture_output=True,
        )
    return SSH_PUBKEY_PATH.read_text().strip()


def _ensure_ssh_config():
    """Write an SSH config file for the VM."""
    SSH_CONFIG_PATH.write_text(
        f"Host vm\n"
        f"    HostName localhost\n"
        f"    Port {DEFAULT_SSH_PORT}\n"
        f"    User {DEFAULT_USER}\n"
        f"    IdentityFile {SSH_KEY_PATH}\n"
        f"    IdentitiesOnly yes\n"
        f"    UserKnownHostsFile {SSH_KNOWN_HOSTS}\n"
        f"    StrictHostKeyChecking accept-new\n"
        f"    PasswordAuthentication no\n"
    )


def create_seed_iso(host_cwd: Path, reset: bool = False):
    if SEED_PATH.exists() and not reset:
        print(f"Seed ISO already exists: {SEED_PATH}")
        return

    pubkey = _ensure_ssh_keypair()
    _ensure_ssh_config()
    password = _ensure_password()
    keys_yaml = f"ssh_authorized_keys:\n        - {pubkey}"

    # cloud-init 'mounts' writes /etc/fstab so the 9p share survives reboots.
    # x-systemd.automount mounts on first access instead of at boot, where the
    # mount can race virtio-9p driver probe ("no channels available").
    mounts_yaml = (
        "mounts:\n"
        f'  - ["hostcwd", "{host_cwd}", "9p",'
        f' "trans=virtio,version=9p2000.L,rw,nofail,x-systemd.automount", "0", "0"]'
    )

    setup_cmds = [
        f"mkdir -p {host_cwd}",
        f"chown {DEFAULT_USER}:{DEFAULT_USER} {host_cwd.parent}",
        f'echo "cd {host_cwd}" >> /home/{DEFAULT_USER}/.bashrc',
        # The e2e test harness reads /dev/kmsg as a regular user; persist the
        # sysctl so it survives reboots (the Makefile's sysctl -w does not).
        'echo "kernel.dmesg_restrict = 0" > /etc/sysctl.d/99-yolofs-test.conf',
        "sysctl -p /etc/sysctl.d/99-yolofs-test.conf",
    ]
    runcmd_yaml = "runcmd:\n" + "\n".join(f"  - {cmd}" for cmd in setup_cmds)

    user_data = (
        "#cloud-config\n"
        f"hostname: {VM_HOSTNAME}\n"
        "manage_etc_hosts: true\n"
        "users:\n"
        f"  - name: {DEFAULT_USER}\n"
        # Match the host uid: the 9p share uses security_model=none, where the
        # guest kernel checks permissions against host file ownership. With the
        # same uid the guest user owns the share and can read/write it normally.
        f"    uid: {os.getuid()}\n"
        "    sudo: ALL=(ALL) NOPASSWD:ALL\n"
        "    shell: /bin/bash\n"
        "    lock_passwd: false\n"
        f'    plain_text_passwd: "{password}"\n'
        f"    {keys_yaml}\n"
        "ssh_pwauth: true\n"
        f"{mounts_yaml}\n"
        f"{runcmd_yaml}\n"
    )

    meta_data = textwrap.dedent(f"""\
        instance-id: {VM_HOSTNAME}-001
        local-hostname: {VM_HOSTNAME}
    """)

    user_data_path = DATA_DIR / "user-data"
    meta_data_path = DATA_DIR / "meta-data"
    user_data_path.write_text(user_data)
    meta_data_path.write_text(meta_data)

    _write_seed_iso(SEED_PATH, [user_data_path, meta_data_path])


def _write_seed_iso(iso_path: Path, files: list[Path]):
    """Build a cloud-init NoCloud seed ISO, replacing cloud-localds with xorriso.

    The NoCloud datasource finds the seed by its volume label (-V CIDATA;
    uppercase to satisfy ISO 9660) and reads user-data/meta-data from the root.
    -R (Rock Ridge) preserves those lowercase hyphenated names, and the sources
    are already named that way, so they can be passed directly.
    """
    if not shutil.which("xorriso"):
        raise RuntimeError("xorriso not found; install it (e.g. apt-get install xorriso)")
    print("Creating cloud-init seed ISO with xorriso...")
    subprocess.run(
        ["xorriso", "-as", "mkisofs", "-o", str(iso_path), "-V", "CIDATA", "-R",
         *(str(f) for f in files)],
        check=True,
    )


def _ssh_cmd() -> list[str]:
    """Base SSH command with common options."""
    return ["ssh", "-F", str(SSH_CONFIG_PATH), "vm"]


def _print_vm_info():
    """Print VM connection info."""
    print(f"  SSH:      {' '.join(_ssh_cmd())}")
    print(f"  Password: {PASSWORD_PATH}")
    print(f"  Log:      {LOG_PATH}")
    print(f"  Stop:     ./vm.py stop")


def is_vm_running():
    """Check if the VM is running via pidfile."""
    if not PID_PATH.exists():
        return False
    pid = int(PID_PATH.read_text().strip())
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        PID_PATH.unlink(missing_ok=True)
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
    print(f"Error: VM not ready after {timeout}s. Check log: {LOG_PATH}")
    sys.exit(1)


def ensure_vm_started():
    """Start the VM with defaults if not already running, and wait until ready."""
    _ensure_ssh_keypair()
    _ensure_ssh_config()
    if is_vm_running():
        print("VM already running.")
        _print_vm_info()
        return
    print("VM not running, starting...")
    download_image()
    create_disk(DEFAULT_DISK_SIZE)
    create_seed_iso(Path.cwd())
    run_vm(DEFAULT_RAM, DEFAULT_CPUS, [])
    wait_for_vm(str(Path.cwd()))


def _detect_accel() -> str:
    """Pick a QEMU accelerator. The guest arch matches the host, so the native
    accelerator (KVM on Linux, HVF on macOS) always applies; TCG is a last
    resort (e.g. Linux without /dev/kvm)."""
    kvm = Path("/dev/kvm")
    if kvm.exists():
        if not os.access(kvm, os.R_OK | os.W_OK):
            print("/dev/kvm not accessible, attempting chmod 666...")
            subprocess.run(["sudo", "chmod", "666", str(kvm)], check=True)
        return "kvm"
    if sys.platform == "darwin":
        return "hvf"
    print("Warning: no KVM; falling back to slow TCG software emulation.")
    return "tcg"


def _find_aarch64_firmware() -> str:
    """Locate the aarch64 UEFI firmware the 'virt' machine boots from."""
    names = ("edk2-aarch64-code.fd", "QEMU_EFI.fd", "AAVMF_CODE.fd")
    dirs = [Path(p) / "share/qemu" for p in ("/opt/homebrew", "/usr/local", "/usr")]
    dirs += [Path("/usr/share/AAVMF"), Path("/usr/share/qemu-efi-aarch64")]
    for d in dirs:
        for name in names:
            if (d / name).exists():
                return str(d / name)
    raise RuntimeError("aarch64 UEFI firmware not found; install qemu.")


def _qemu_base() -> tuple[str, list[str]]:
    """QEMU binary plus machine/cpu/firmware args for the host architecture.

    -cpu host gives near-native speed under a hardware accelerator; the TCG
    fallback needs a concrete model, so it uses -cpu max instead.
    """
    accel = _detect_accel()
    cpu = "host" if accel != "tcg" else "max"
    if GUEST_ARCH == "arm64":
        # 'virt' is the standard arm64 board; it boots via UEFI firmware.
        return "qemu-system-aarch64", [
            "-machine", f"virt,accel={accel}", "-cpu", cpu,
            "-bios", _find_aarch64_firmware(),
        ]
    return "qemu-system-x86_64", ["-machine", f"q35,accel={accel}", "-cpu", cpu]


def run_vm(
    ram: str,
    cpus: int,
    extra_args: list[str],
    foreground: bool = False,
):
    qemu, base_args = _qemu_base()
    cmd = [
        qemu,
        *base_args,
        "-m", ram,
        "-smp", str(cpus),
        "-drive", f"file={DISK_PATH},format=qcow2,if=virtio",
        "-drive", f"file={SEED_PATH},format=raw,if=virtio",
        "-net", "nic,model=virtio",
        "-net", f"user,hostfwd=tcp:127.0.0.1:{DEFAULT_SSH_PORT}-:22",
        "-virtfs", f"local,path={Path.cwd()},mount_tag=hostcwd,security_model=none",
    ]

    if foreground:
        cmd += ["-nographic"]
        cmd += extra_args
        print(f"Starting VM ({' '.join(_ssh_cmd())})...")
        print(f"  RAM={ram}  CPUs={cpus}")
        print("Press Ctrl-A X to exit QEMU console.\n")
        os.execvp(cmd[0], cmd)

    cmd += [
        "-display", "none",
        "-serial", f"file:{LOG_PATH}",
        "-monitor", "none",
        "-daemonize",
        "-pidfile", str(PID_PATH),
    ]
    cmd += extra_args
    print(f"Starting VM in background...")
    subprocess.run(cmd, check=True)
    _print_vm_info()


def stop_vm():
    if not PID_PATH.exists():
        print("No pidfile found; VM may not be running.")
        return
    pid = int(PID_PATH.read_text().strip())
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
    PID_PATH.unlink(missing_ok=True)


def ssh_vm(ssh_args: list[str]):
    host_cwd = str(Path.cwd())
    cmd = _ssh_cmd()
    if ssh_args:
        cmd += [f"cd {host_cwd} && source ~/.profile 2>/dev/null &&"] + ssh_args
    else:
        cmd += ["-t", f"cd {host_cwd} && exec $SHELL -l"]
    os.execvp(cmd[0], cmd)


def main():
    # vm.py is a host-side tool; running it inside the guest would just nest VMs.
    if platform.node() == VM_HOSTNAME:
        print("Error: vm.py is running inside the YoloFS VM; run it on the host.")
        sys.exit(1)
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

    # --- reset ---
    sub.add_parser("reset", help="Stop VM and recreate disk and seed images")

    # --- download ---
    sub.add_parser("download", help="Download the base cloud image only")

    args = parser.parse_args()

    DATA_DIR.mkdir(parents=True, exist_ok=True)

    if args.command == "download":
        download_image()
    elif args.command == "stop":
        stop_vm()
    elif args.command == "reset":
        stop_vm()
        download_image()
        create_disk(DEFAULT_DISK_SIZE, reset=True)
        PASSWORD_PATH.unlink(missing_ok=True)
        create_seed_iso(Path.cwd(), reset=True)
        SSH_KNOWN_HOSTS.unlink(missing_ok=True)
        print("Reset complete. Run './vm.py start' to boot.")
    elif args.command in ("start", "restart"):
        if args.command == "restart":
            stop_vm()
        elif is_vm_running():
            print(f"VM is already running.")
            _print_vm_info()
            sys.exit(1)
        download_image()
        create_disk(args.disk_size)
        create_seed_iso(Path.cwd())
        run_vm(args.ram, args.cpus, args.extra, foreground=args.foreground)


if __name__ == "__main__":
    main()
