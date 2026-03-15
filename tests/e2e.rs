/// agfs integration test harness.
///
/// These tests exercise the full kernel module + CLI stack.
/// Must be run as root with the agfs module loaded:
///
///   sudo -E cargo test -p agfs --test integration -- --test-threads=1
///
/// The --test-threads=1 is required because each test mounts/unmounts agfs.
mod helpers;

mod fs;
mod cli;
mod perm;
mod internals;
