use crate::helpers::AgfsSession;
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn commit_modified_file() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "committed\n").unwrap();

    let output = s.cli(&["commit"]).expect("commit");
    assert!(output.contains("Committed 1 change"), "output: {output}");

    // Base file now has the committed content
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "committed\n"
    );
}

#[test]
fn commit_new_file() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("brandnew.txt"), "new\n").unwrap();

    let output = s.cli(&["commit"]).expect("commit");
    assert!(output.contains("Committed 1 change"), "output: {output}");

    // New file now in base
    assert_eq!(
        fs::read_to_string(s.base_path("brandnew.txt")).unwrap(),
        "new\n"
    );
}

#[test]
fn commit_multiple_changes() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "changed\n").unwrap();
    fs::write(s.mnt_path("newfile.txt"), "new\n").unwrap();

    let output = s.cli(&["commit"]).expect("commit");
    assert!(output.contains("Committed 2 change"), "output: {output}");

    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "changed\n"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("newfile.txt")).unwrap(),
        "new\n"
    );
}

#[test]
fn commit_nothing() {
    let s = AgfsSession::new().expect("session setup");

    let output = s.cli(&["commit"]).expect("commit");
    assert!(output.contains("Nothing to commit"), "output: {output}");
}

/// Delete a directory, create a file with the same name, commit.
/// The commit should replace the directory with the file in base.
#[test]
fn commit_replace_dir_with_file() {
    let s = AgfsSession::new().expect("session setup");

    // subdir/ exists in base as a directory
    assert!(s.base_path("subdir").is_dir());

    // Remove the directory and create a file with the same name
    fs::remove_dir_all(s.mnt_path("subdir")).expect("rmdir");
    fs::write(s.mnt_path("subdir"), "now a file\n").expect("write");

    let content = fs::read_to_string(s.mnt_path("subdir")).expect("read");
    assert_eq!(content, "now a file\n");

    s.cli(&["commit"]).expect("commit");

    // Base should now have a file, not a directory
    assert!(
        !s.base_path("subdir").is_dir(),
        "base subdir should no longer be a directory after commit"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("subdir")).unwrap(),
        "now a file\n",
        "base should have the file content"
    );
}

/// Committing a modified read-only file should preserve its 0o444 mode.
#[test]
fn commit_preserves_readonly_mode() {
    use std::os::unix::fs::MetadataExt;

    let s = AgfsSession::new().expect("session setup");

    let base = s.base_path("hello.txt");
    fs::set_permissions(&base, fs::Permissions::from_mode(0o444)).expect("chmod");

    fs::write(s.mnt_path("hello.txt"), "committed\n").expect("write");
    s.cli(&["commit"]).expect("commit");

    let meta = fs::metadata(&base).unwrap();
    assert_eq!(
        meta.mode() & 0o777,
        0o444,
        "committed file should preserve original mode 0444, got {:o}",
        meta.mode() & 0o777
    );
    assert_eq!(fs::read_to_string(&base).unwrap(), "committed\n");
}

/// Committing a modified executable file should preserve its 0o755 mode.
#[test]
fn commit_preserves_executable_mode() {
    use std::os::unix::fs::MetadataExt;

    let s = AgfsSession::new().expect("session setup");

    // test.sh is seeded with mode 0o755
    fs::write(s.mnt_path("test.sh"), "#!/bin/sh\necho committed\n").expect("write");
    s.cli(&["commit"]).expect("commit");

    let base = s.base_path("test.sh");
    let meta = fs::metadata(&base).unwrap();
    assert_eq!(
        meta.mode() & 0o777,
        0o755,
        "committed executable should preserve mode 0755, got {:o}",
        meta.mode() & 0o777
    );
}

/// Committing a newly created file should keep its default mode (not
/// inherit from some other file).
#[test]
fn commit_new_file_has_default_mode() {
    use std::os::unix::fs::MetadataExt;

    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("brand_new.txt"), "new\n").expect("write");
    s.cli(&["commit"]).expect("commit");

    let base = s.base_path("brand_new.txt");
    let meta = fs::metadata(&base).unwrap();
    let mode = meta.mode() & 0o777;
    // New files get the default mode from the creating process's umask
    assert!(
        mode == 0o644 || mode == 0o664,
        "new file should have reasonable default mode, got {:o}",
        mode
    );
}

/// Commit after restore should only apply live changes (checkpoint state),
/// excluding dead-zone mutations.
#[test]
fn commit_after_restore_excludes_dead_zone() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "wanted\n").unwrap();
    fs::write(s.mnt_path("keep.txt"), "keep\n").unwrap();
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

    // Post-checkpoint changes (will be in dead zone after restore)
    fs::write(s.mnt_path("hello.txt"), "unwanted\n").unwrap();
    fs::write(s.mnt_path("dead.txt"), "dead\n").unwrap();
    fs::remove_file(s.mnt_path("keep.txt")).unwrap();

    s.cli(&["restore", "chk1"]).expect("restore");
    s.cli(&["commit"]).expect("commit");

    // Base should have checkpoint state, not post-checkpoint mutations
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "wanted\n",
        "base should have checkpoint content"
    );
    assert_eq!(
        fs::read_to_string(s.base_path("keep.txt")).unwrap(),
        "keep\n",
        "deleted-after-checkpoint file should be in base"
    );
    assert!(
        !s.base_path("dead.txt").exists(),
        "post-checkpoint file should NOT be in base"
    );
}
