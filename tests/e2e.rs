/// yolofs integration test harness.
///
/// These tests exercise the full kernel module + CLI stack.
///
///   cargo test -p yolofs --test e2e -- --test-threads=1
///
/// --test-threads=1 is required because each test mounts/unmounts yolofs.
mod helpers;

mod cli;
mod fs;
mod internals;
mod perm;
