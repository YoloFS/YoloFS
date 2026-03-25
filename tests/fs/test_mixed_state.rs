use crate::helpers::AgfsSession;
use std::fs;

// ── Base rename + staged create at old name ─────────────────────────

/// Rename a base file away, then create a staged file at the old name.
/// Both the renamed base file and the new staged file should coexist.
#[test]
fn rename_base_then_create_staged_at_old_name() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename base");
    fs::write(s.mnt_path("hello.txt"), "staged content\n").expect("create staged");

    // Renamed base file at new path
    assert_eq!(
        fs::read_to_string(s.mnt_path("moved.txt")).unwrap(),
        "base content\n"
    );

    // New staged file at old path
    assert_eq!(
        fs::read_to_string(s.mnt_path("hello.txt")).unwrap(),
        "staged content\n"
    );

    // Commit materializes both
    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("moved.txt")).unwrap(),
        "base content\n"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "staged content\n"
    );
}

// ── Staged file rename over base file ───────────────────────────────

/// Create a staged file, then rename it over an existing base file.
#[test]
fn rename_staged_over_base() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("new.txt"), "staged\n").expect("create staged");
    fs::rename(s.mnt_path("new.txt"), s.mnt_path("hello.txt")).expect("rename staged over base");

    // Base file is replaced by staged content
    assert_eq!(
        fs::read_to_string(s.mnt_path("hello.txt")).unwrap(),
        "staged\n"
    );

    // Old staged name is gone
    assert!(fs::read_to_string(s.mnt_path("new.txt")).is_err());

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "staged\n"
    );
    assert!(!s.base_path("new.txt").exists());
}

/// Rename a base file over another base file.
#[test]
fn rename_base_over_base() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("multi.txt")).expect("rename base over base");

    // Destination has source content
    assert_eq!(
        fs::read_to_string(s.mnt_path("multi.txt")).unwrap(),
        "base content\n"
    );

    // Source is gone
    assert!(fs::read_to_string(s.mnt_path("hello.txt")).is_err());

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("multi.txt")).unwrap(),
        "base content\n"
    );
    assert!(!s.base_path("hello.txt").exists());
}

// ── Delete base + create staged at same path ────────────────────────

/// Delete a base file, then create a staged file at the same path.
#[test]
fn delete_base_then_create_staged() {
    let s = AgfsSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).expect("delete base");
    assert!(fs::read_to_string(s.mnt_path("hello.txt")).is_err());

    fs::write(s.mnt_path("hello.txt"), "replacement\n").expect("create staged");

    assert_eq!(
        fs::read_to_string(s.mnt_path("hello.txt")).unwrap(),
        "replacement\n"
    );

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "replacement\n"
    );
}

// ── Mixed rename chains ─────────────────────────────────────────────

/// Rename base A→B, create staged A, modify base C via mount. All three
/// should be independently visible and committable.
#[test]
fn mixed_rename_create_modify() {
    let s = AgfsSession::new().expect("session setup");

    // Rename base file to new name
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename base");

    // Create a new staged file at the old name
    fs::write(s.mnt_path("hello.txt"), "new staged\n").expect("create staged");

    // Modify another base file (triggers COW)
    fs::write(s.mnt_path("multi.txt"), "modified\n").expect("modify base");

    // All three are visible
    assert_eq!(
        fs::read_to_string(s.mnt_path("moved.txt")).unwrap(),
        "base content\n"
    );
    assert_eq!(
        fs::read_to_string(s.mnt_path("hello.txt")).unwrap(),
        "new staged\n"
    );
    assert_eq!(
        fs::read_to_string(s.mnt_path("multi.txt")).unwrap(),
        "modified\n"
    );

    s.cli(&["commit"]).expect("commit");

    assert_eq!(
        fs::read_to_string(s.base_path("moved.txt")).unwrap(),
        "base content\n"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "new staged\n"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("multi.txt")).unwrap(),
        "modified\n"
    );
}

// ── Readdir with mixed state ────────────────────────────────────────

/// After mixed operations, readdir should show the correct set of entries.
#[test]
fn readdir_mixed_base_and_staged() {
    let s = AgfsSession::new().expect("session setup");

    // Delete a base file
    fs::remove_file(s.mnt_path("multi.txt")).expect("delete base");

    // Rename another base file
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("renamed.txt")).expect("rename base");

    // Create a staged file
    fs::write(s.mnt_path("brand_new.txt"), "fresh\n").expect("create staged");

    // Collect directory entries
    let entries: Vec<String> = fs::read_dir(s.mnt_path(""))
        .expect("readdir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| !name.starts_with('.'))
        .collect();

    // Should see: renamed.txt, brand_new.txt, subdir, test.sh
    assert!(entries.contains(&"renamed.txt".to_string()), "missing renamed.txt: {entries:?}");
    assert!(entries.contains(&"brand_new.txt".to_string()), "missing brand_new.txt: {entries:?}");
    assert!(entries.contains(&"subdir".to_string()), "missing subdir: {entries:?}");
    assert!(entries.contains(&"test.sh".to_string()), "missing test.sh: {entries:?}");

    // Should NOT see: hello.txt (renamed), multi.txt (deleted)
    assert!(!entries.contains(&"hello.txt".to_string()), "hello.txt should be gone: {entries:?}");
    assert!(!entries.contains(&"multi.txt".to_string()), "multi.txt should be gone: {entries:?}");
}

// ── Abort after mixed operations ────────────────────────────────────

/// Mixed operations then abort: base should be completely untouched.
#[test]
fn mixed_operations_abort() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
    fs::write(s.mnt_path("brand_new.txt"), "staged\n").expect("create");
    fs::remove_file(s.mnt_path("multi.txt")).expect("delete");
    fs::write(s.mnt_path("subdir/deep.txt"), "modified\n").expect("modify");

    s.cli(&["abort"]).expect("abort");

    // Base is untouched
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("multi.txt")).unwrap(),
        "line1\nline2\n"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("subdir/deep.txt")).unwrap(),
        "nested\n"
    );
    assert!(!s.base_path("moved.txt").exists());
    assert!(!s.base_path("brand_new.txt").exists());
}

// ── Rename staged file to new directory ─────────────────────────────

/// Create a staged file, then rename it into a subdirectory.
#[test]
fn rename_staged_into_subdir() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("top.txt"), "staged top\n").expect("create");
    fs::rename(s.mnt_path("top.txt"), s.mnt_path("subdir/top.txt")).expect("rename into subdir");

    assert!(fs::read_to_string(s.mnt_path("top.txt")).is_err());
    assert_eq!(
        fs::read_to_string(s.mnt_path("subdir/top.txt")).unwrap(),
        "staged top\n"
    );

    s.cli(&["commit"]).expect("commit");

    assert!(!s.base_path("top.txt").exists());
    assert_eq!(
        fs::read_to_string(s.base_path("subdir/top.txt")).unwrap(),
        "staged top\n"
    );
}

/// Rename a base file out of a subdirectory to the top level.
#[test]
fn rename_base_out_of_subdir() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(
        s.mnt_path("subdir/deep.txt"),
        s.mnt_path("promoted.txt"),
    )
    .expect("rename out of subdir");

    assert!(fs::read_to_string(s.mnt_path("subdir/deep.txt")).is_err());
    assert_eq!(
        fs::read_to_string(s.mnt_path("promoted.txt")).unwrap(),
        "nested\n"
    );

    s.cli(&["commit"]).expect("commit");

    assert!(!s.base_path("subdir/deep.txt").exists());
    assert_eq!(
        fs::read_to_string(s.base_path("promoted.txt")).unwrap(),
        "nested\n"
    );
}

// ── Modify then rename (COW + redirect) ─────────────────────────────

/// Write to a base file (triggers COW → staged inode), then rename it.
/// The staged inode should follow the rename.
#[test]
fn modify_base_then_rename() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("modify base (COW)");
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename staged inode");

    assert!(fs::read_to_string(s.mnt_path("hello.txt")).is_err());
    assert_eq!(
        fs::read_to_string(s.mnt_path("moved.txt")).unwrap(),
        "modified\n"
    );

    s.cli(&["commit"]).expect("commit");

    assert!(!s.base_path("hello.txt").exists());
    assert_eq!(
        fs::read_to_string(s.base_path("moved.txt")).unwrap(),
        "modified\n"
    );
}
