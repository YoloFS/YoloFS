use crate::helpers::AgfsSession;
use std::fs;

#[test]
fn readdir() {
    let s = AgfsSession::new().expect("session setup");

    let entries: Vec<String> = fs::read_dir(s.mnt_path(""))
        .expect("readdir")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();

    assert!(entries.contains(&"hello.txt".to_string()));
    assert!(entries.contains(&"multi.txt".to_string()));
    assert!(entries.contains(&"subdir".to_string()));
}

// ── readdir with staging changes (file.c: agfs_readdir) ──

/// Newly created files should appear in readdir.
#[test]
fn readdir_shows_created_file() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("newfile.txt"), "new\n").expect("create");

    let entries: Vec<String> = fs::read_dir(s.mnt_path(""))
        .expect("readdir")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();

    assert!(
        entries.contains(&"newfile.txt".to_string()),
        "readdir should include newly created file, got: {entries:?}"
    );
    // Base files should still be listed
    assert!(entries.contains(&"hello.txt".to_string()));
}

/// Deleted files should be hidden from readdir.
#[test]
fn readdir_hides_deleted_file() {
    let s = AgfsSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).expect("unlink");

    let entries: Vec<String> = fs::read_dir(s.mnt_path(""))
        .expect("readdir")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();

    assert!(
        !entries.contains(&"hello.txt".to_string()),
        "readdir should not include deleted file, got: {entries:?}"
    );
    // Other files still listed
    assert!(entries.contains(&"multi.txt".to_string()));
}

/// After rename, readdir shows new name and hides old name.
/// The kernel creates a staging stub at the new path so readdir
/// discovers it when merging staging + base.
#[test]
fn readdir_after_rename() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("renamed.txt")).expect("rename");

    let entries: Vec<String> = fs::read_dir(s.mnt_path(""))
        .expect("readdir")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();

    // Old name should be hidden (via renamed_children on parent)
    assert!(
        !entries.contains(&"hello.txt".to_string()),
        "readdir should not include old name after rename, got: {entries:?}"
    );

    // New name should appear (staging stub)
    assert!(
        entries.contains(&"renamed.txt".to_string()),
        "readdir should include renamed file, got: {entries:?}"
    );

    // Content still accessible via the new name
    let content =
        fs::read_to_string(s.mnt_path("renamed.txt")).expect("renamed file should be readable");
    assert_eq!(content, "base content\n");
}

/// Newly created directories appear in readdir.
#[test]
fn readdir_shows_created_dir() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("newdir")).expect("mkdir");

    let entries: Vec<String> = fs::read_dir(s.mnt_path(""))
        .expect("readdir")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();

    assert!(
        entries.contains(&"newdir".to_string()),
        "readdir should include new directory, got: {entries:?}"
    );
}

/// readdir on a subdirectory lists its contents.
#[test]
fn readdir_subdir() {
    let s = AgfsSession::new().expect("session setup");

    let entries: Vec<String> = fs::read_dir(s.mnt_path("subdir"))
        .expect("readdir subdir")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();

    assert!(
        entries.contains(&"deep.txt".to_string()),
        "readdir on subdir should list deep.txt, got: {entries:?}"
    );
}

/// readdir on a newly created dir with files inside it.
#[test]
fn readdir_new_dir_with_files() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("newdir")).expect("mkdir");
    fs::write(s.mnt_path("newdir/a.txt"), "a\n").expect("write a");
    fs::write(s.mnt_path("newdir/b.txt"), "b\n").expect("write b");

    let entries: Vec<String> = fs::read_dir(s.mnt_path("newdir"))
        .expect("readdir newdir")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();

    assert!(entries.contains(&"a.txt".to_string()), "got: {entries:?}");
    assert!(entries.contains(&"b.txt".to_string()), "got: {entries:?}");
}

#[test]
fn readdir_many_files() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("bigdir")).expect("mkdir");
    for i in 0..100 {
        fs::write(s.mnt_path(&format!("bigdir/file_{i:03}.txt")), format!("content {i}\n"))
            .expect("write");
    }

    let entries: Vec<String> = fs::read_dir(s.mnt_path("bigdir"))
        .expect("readdir")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();

    assert_eq!(entries.len(), 100, "expected 100 entries, got {}", entries.len());
    for i in 0..100 {
        let name = format!("file_{i:03}.txt");
        assert!(entries.contains(&name), "missing {name} in readdir");
    }
}
