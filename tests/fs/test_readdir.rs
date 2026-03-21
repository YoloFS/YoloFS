use crate::helpers::AgfsSession;
use std::fs;
use std::os::unix::fs::DirEntryExt;

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

// ── readdir with staged changes (file.c: agfs_readdir) ──

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
/// The kernel creates a staged stub at the new path so readdir
/// discovers it when merging dirents + base.
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

    // New name should appear (staged stub)
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

// ── readdir d_type correctness ──

/// DirEntry::file_type() uses d_type from getdents64 on Linux.
/// Overridden directories must report as directories, not regular files.
#[test]
fn readdir_dtype_dir() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("dtype_dir")).expect("mkdir");

    for entry in fs::read_dir(s.mnt_path("")).expect("readdir") {
        let entry = entry.expect("entry");
        if entry.file_name() == "dtype_dir" {
            let ft = entry.file_type().expect("file_type");
            assert!(
                ft.is_dir(),
                "readdir d_type for created directory should be dir, got: {ft:?}"
            );
            return;
        }
    }
    panic!("dtype_dir not found in readdir");
}

/// Overridden symlinks must report as symlinks in readdir d_type.
#[test]
fn readdir_dtype_symlink() {
    let s = AgfsSession::new().expect("session setup");

    std::os::unix::fs::symlink("hello.txt", s.mnt_path("dtype_link")).expect("symlink");

    for entry in fs::read_dir(s.mnt_path("")).expect("readdir") {
        let entry = entry.expect("entry");
        if entry.file_name() == "dtype_link" {
            let ft = entry.file_type().expect("file_type");
            assert!(
                ft.is_symlink(),
                "readdir d_type for created symlink should be symlink, got: {ft:?}"
            );
            return;
        }
    }
    panic!("dtype_link not found in readdir");
}

/// Overridden regular files should still report as regular files.
#[test]
fn readdir_dtype_file() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("dtype_file.txt"), "data\n").expect("create");

    for entry in fs::read_dir(s.mnt_path("")).expect("readdir") {
        let entry = entry.expect("entry");
        if entry.file_name() == "dtype_file.txt" {
            let ft = entry.file_type().expect("file_type");
            assert!(
                ft.is_file(),
                "readdir d_type for created file should be file, got: {ft:?}"
            );
            return;
        }
    }
    panic!("dtype_file.txt not found in readdir");
}

#[test]
fn readdir_many_files() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("bigdir")).expect("mkdir");
    for i in 0..100 {
        fs::write(
            s.mnt_path(&format!("bigdir/file_{i:03}.txt")),
            format!("content {i}\n"),
        )
        .expect("write");
    }

    let entries: Vec<String> = fs::read_dir(s.mnt_path("bigdir"))
        .expect("readdir")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();

    assert_eq!(
        entries.len(),
        100,
        "expected 100 entries, got {}",
        entries.len()
    );
    for i in 0..100 {
        let name = format!("file_{i:03}.txt");
        assert!(entries.contains(&name), "missing {name} in readdir");
    }
}

// ── readdir inode number correctness ──

/// A newly created file must have a non-zero inode number in readdir.
/// Regression: agfs_emit_dirents passed ino=0 to dir_emit for staged
/// entries, which caused some kernels to silently drop the entry.
#[test]
fn readdir_created_file_has_nonzero_ino() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("newfile.txt"), "data\n").expect("create");

    for entry in fs::read_dir(s.mnt_path("")).expect("readdir") {
        let entry = entry.expect("entry");
        if entry.file_name() == "newfile.txt" {
            let ino = entry.ino();
            assert_ne!(ino, 0, "staged file should have non-zero ino in readdir");
            return;
        }
    }
    panic!("newfile.txt not found in readdir");
}

/// A renamed file must have a non-zero inode number in readdir.
/// Regression: renamed entries (base_path redirects) had ino=0 in
/// dir_emit because de->ino is 0 for redirects.
#[test]
fn readdir_renamed_file_has_nonzero_ino() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("renamed.txt")).expect("rename");

    for entry in fs::read_dir(s.mnt_path("")).expect("readdir") {
        let entry = entry.expect("entry");
        if entry.file_name() == "renamed.txt" {
            let ino = entry.ino();
            assert_ne!(ino, 0, "renamed file should have non-zero ino in readdir");
            return;
        }
    }
    panic!("renamed.txt not found in readdir");
}

// ── d_type after rename (link dirent encoding) ──

/// A renamed (not yet COW'd) file should still report as a regular file.
/// This exercises the link variant's d_type encoding.
#[test]
fn readdir_dtype_renamed_file() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("link_file.txt")).expect("rename");

    for entry in fs::read_dir(s.mnt_path("")).expect("readdir") {
        let entry = entry.expect("entry");
        if entry.file_name() == "link_file.txt" {
            let ft = entry.file_type().expect("file_type");
            assert!(
                ft.is_file(),
                "renamed (link) file should have d_type = file, got: {ft:?}"
            );
            return;
        }
    }
    panic!("link_file.txt not found in readdir");
}

/// A renamed directory should still report as a directory.
/// This exercises the link variant's d_type encoding for directories.
#[test]
fn readdir_dtype_renamed_dir() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("subdir"), s.mnt_path("link_dir")).expect("rename dir");

    for entry in fs::read_dir(s.mnt_path("")).expect("readdir") {
        let entry = entry.expect("entry");
        if entry.file_name() == "link_dir" {
            let ft = entry.file_type().expect("file_type");
            assert!(
                ft.is_dir(),
                "renamed (link) directory should have d_type = dir, got: {ft:?}"
            );
            return;
        }
    }
    panic!("link_dir not found in readdir");
}

/// All three d_types in one directory: regular, directory, symlink.
/// Verifies the 2-bit d_type encoding handles all variants correctly.
#[test]
fn readdir_dtype_all_three_types() {
    let s = AgfsSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("typedir")).expect("mkdir container");
    fs::write(s.mnt_path("typedir/reg.txt"), "regular\n").expect("create regular");
    fs::create_dir(s.mnt_path("typedir/sub")).expect("mkdir sub");
    std::os::unix::fs::symlink("reg.txt", s.mnt_path("typedir/lnk")).expect("symlink");

    let mut found = [false; 3]; // [regular, directory, symlink]

    for entry in fs::read_dir(s.mnt_path("typedir")).expect("readdir") {
        let entry = entry.expect("entry");
        let ft = entry.file_type().expect("file_type");
        let name = entry.file_name().to_string_lossy().to_string();

        match name.as_str() {
            "reg.txt" => {
                assert!(ft.is_file(), "reg.txt should be file, got: {ft:?}");
                found[0] = true;
            }
            "sub" => {
                assert!(ft.is_dir(), "sub should be dir, got: {ft:?}");
                found[1] = true;
            }
            "lnk" => {
                assert!(ft.is_symlink(), "lnk should be symlink, got: {ft:?}");
                found[2] = true;
            }
            _ => panic!("unexpected entry: {name}"),
        }
    }
    assert!(found[0], "reg.txt not found in readdir");
    assert!(found[1], "sub not found in readdir");
    assert!(found[2], "lnk not found in readdir");
}
