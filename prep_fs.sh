#!/usr/bin/env bash
set -euo pipefail

data_root="${1:-/tmp/agentctl}"

rm -rf "$data_root"
mkdir -p "$data_root/project"

# `file1` is a regular file in the project directory
printf "file1\nfile1\nfile1\n" > "$data_root/project/file1"

# `file2` is a regular file in the parent directory
printf "file2\nfile2\nfile2\n" > "$data_root/file2"

# `file3` is a symlink to `file3` in the parent directory
printf "file3\nfile3\nfile3\n" > "$data_root/file3"
ln -sfn "../file2" "$data_root/project/file3"

# `dir1` is a symlink to `dir1` in the parent directory
mkdir -p "$data_root/dir1"
ln -sfn "../dir1" "$data_root/project/dir1"

# `file4` is a regular file in the symlink directory
printf "file4\nfile4\nfile4\n" > "$data_root/dir1/file4"

echo "Filesystem prepared at $data_root"
