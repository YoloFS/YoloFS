#!/usr/bin/env bash
set -euo pipefail

data_root="${1:?Usage: prep_fs.sh DATA_ROOT}"

rm -rf "$data_root"

mkdir -p "$data_root/project"
echo "file1" > "$data_root/project/file1"

mkdir -p "$data_root/dir1"
echo "file3" > "$data_root/dir1/file3"
ln -sfn "../dir1" "$data_root/project/dir1"

echo "file2" > "$data_root/file2"
ln -sfn "../file2" "$data_root/project/file2"
