/// agfs integration test harness.
///
/// These tests exercise the full kernel module + CLI stack.
/// Must be run as root with the agfs module loaded:
///
///   sudo -E cargo test -p agfs --test integration -- --test-threads=1
///
/// The --test-threads=1 is required because each test mounts/unmounts agfs.
mod helpers;

mod test_abort;
mod test_commit;
mod test_create;
mod test_delete;
mod test_diff;
mod test_inode;
mod test_journal;
mod test_lifecycle;
mod test_mkdir;
mod test_mount;
mod test_multiple_writes;
mod test_perm;
mod test_read;
mod test_rename;
mod test_rmdir;
mod test_rules;
mod test_run;
mod test_snapshot;
mod test_status;
mod test_symlink;
mod test_truncate_append;
mod test_unmount;
mod test_watch;
mod test_write_cow;
