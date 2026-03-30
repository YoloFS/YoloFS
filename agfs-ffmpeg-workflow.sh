#!/usr/bin/env bash

set -euo pipefail

# AgFS + FFmpeg workflow.
#
# This script is written as executable notes: every section explains why it
# exists, then runs the command that proved out during the session.
#
# What it does:
# 1. mounts AgFS if needed
# 2. bind-mounts host /run into the session so DNS works inside AgFS
# 3. bind-mounts host /dev/pts into the session so sudo can allocate a PTY if needed
# 4. fetches and extracts FFmpeg inside AgFS
# 5. configures and builds FFmpeg inside AgFS using normal /tmp
# 6. installs it to /usr/local inside the staged AgFS view
# 7. generates a sample input, transcodes it, and probes the output
#
# Important:
# - No agfs commit is performed here.
# - /usr/local writes inside AgFS still require sudo because Unix ownership
#   still applies even when AgFS permissions allow the path.
# - The verified transcode uses the built-in mpeg4 encoder, not libx264.

ROOT="/users/ljx/ffmpeg-test"
SESSION_DIR="$ROOT/.agfs"
MNT="$SESSION_DIR/mnt"

FFMPEG_VERSION="7.1.1"
FFMPEG_TARBALL="ffmpeg-$FFMPEG_VERSION.tar.xz"
FFMPEG_URL="https://ffmpeg.org/releases/$FFMPEG_TARBALL"
FFMPEG_SRC_DIR="$ROOT/ffmpeg-$FFMPEG_VERSION"

INPUT_MP4="$ROOT/input.mp4"
OUTPUT_MP4="$ROOT/output.mp4"

note() {
  printf '\n# %s\n' "$*"
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    printf 'missing required command: %s\n' "$1" >&2
    exit 1
  }
}

need_cmd agfs
need_cmd curl
need_cmd tar
need_cmd make
need_cmd sudo

note "Mount AgFS if this workspace is not mounted yet"
if [[ ! -d "$MNT" ]]; then
  agfs mount
fi

note "Bind host /run into the AgFS mount so /etc/resolv.conf can resolve into /run/systemd/resolve"
sudo mkdir -p "$MNT/run"
if ! mountpoint -q "$MNT/run"; then
  sudo mount --bind /run "$MNT/run"
fi

note "Bind host /dev/pts into the AgFS mount so sudo has a working PTY device tree"
sudo mkdir -p "$MNT/dev/pts"
if ! mountpoint -q "$MNT/dev/pts"; then
  sudo mount --bind /dev/pts "$MNT/dev/pts"
fi

note "Fetch, extract, build, install, and verify inside AgFS"
agfs exec -- /usr/bin/env \
  PATH=/usr/local/bin:/usr/bin:/bin \
  HOME=/users/ljx \
  SHELL=/bin/sh \
  /usr/bin/bash -lc '
    set -eux

    if [[ ! -f "$2" ]]; then
      curl -L "$1" -o "$2"
    fi

    if [[ ! -d "$3" ]]; then
      tar -xf "$2" -C "$4"
    fi

    cd "$3"

    # The source tree itself must be executable under AgFS because configure
    # builds and runs probe binaries in-place.
    ./configure --prefix=/usr/local --disable-debug --disable-doc

    make -j"$(nproc)"

    # /usr/local is root-owned on the host, so install inside AgFS still needs
    # sudo even when the AgFS rule for /usr/local is allow.
    /usr/bin/sudo -n -- make install

    # Generate a small deterministic input file inside the staged worktree.
    /usr/bin/sudo -n -- /usr/local/bin/ffmpeg \
      -f lavfi -i testsrc=size=128x96:rate=1 -t 2 \
      "$5" -y

    # Use the built-in mpeg4 encoder for verification. This build did not have
    # libx264 enabled.
    /usr/bin/sudo -n -- /usr/local/bin/ffmpeg \
      -i "$5" \
      -c:v mpeg4 \
      "$6" -y

    /usr/bin/sudo -n -- /usr/local/bin/ffprobe \
      -v error -show_entries format=duration,size \
      -of default=noprint_wrappers=1 \
      "$6"
  ' sh "$FFMPEG_URL" "$ROOT/$FFMPEG_TARBALL" "$FFMPEG_SRC_DIR" "$ROOT" "$INPUT_MP4" "$OUTPUT_MP4"

note "Show the staged artifacts inside the AgFS mount"
ls -l \
  "$MNT/usr/local/bin/ffmpeg" \
  "$MNT/usr/local/bin/ffprobe" \
  "$MNT/users/ljx/ffmpeg-test/input.mp4" \
  "$MNT/users/ljx/ffmpeg-test/output.mp4"

note "Done. Everything is still staged inside AgFS until you run agfs commit"
