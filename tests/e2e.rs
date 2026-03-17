/// agfs integration test harness.
///
/// These tests exercise the full kernel module + CLI stack.
///
///   cargo test -p agfs --test e2e -- --test-threads=1
///
/// --test-threads=1 is required because each test mounts/unmounts agfs.
mod helpers;

mod cli;
mod fs;
mod internals;
mod perm;
