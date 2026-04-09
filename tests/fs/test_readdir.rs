use crate::helpers::YoloSession;
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::DirEntryExt;

#[test]
fn readdir() {
    let s = YoloSession::new().expect("session setup");

    let entries: Vec<String> = fs::read_dir(s.mnt_path(""))
        .expect("readdir")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();

    assert!(entries.contains(&"hello.txt".to_string()));
    assert!(entries.contains(&"multi.txt".to_string()));
    assert!(entries.contains(&"subdir".to_string()));
}

// ── readdir with staged changes (file.c: yolo_readdir) ──

/// Newly created files should appear in readdir.
#[test]
fn readdir_shows_created_file() {
    let s = YoloSession::new().expect("session setup");

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
    let s = YoloSession::new().expect("session setup");

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
    let s = YoloSession::new().expect("session setup");

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
    let s = YoloSession::new().expect("session setup");

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
    let s = YoloSession::new().expect("session setup");

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
    let s = YoloSession::new().expect("session setup");

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
    let s = YoloSession::new().expect("session setup");

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
    let s = YoloSession::new().expect("session setup");

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
    let s = YoloSession::new().expect("session setup");

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
    let s = YoloSession::new().expect("session setup");

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
/// Regression: yolo_emit_dirents passed ino=0 to dir_emit for staged
/// entries, which caused some kernels to silently drop the entry.
#[test]
fn readdir_created_file_has_nonzero_ino() {
    let s = YoloSession::new().expect("session setup");

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
    let s = YoloSession::new().expect("session setup");

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
    let s = YoloSession::new().expect("session setup");

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
    let s = YoloSession::new().expect("session setup");

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
    let s = YoloSession::new().expect("session setup");

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

// ── small-buffer getdents64 tests (exercises multi-call readdir) ──

/// Read all directory entries using raw getdents64 with a small buffer,
/// forcing multiple kernel re-entries. Returns names in emission order
/// (may contain duplicates if the kernel is buggy) and the number of
/// getdents64 calls made (excluding the final zero-return).
fn readdir_small_buf(path: &std::path::Path) -> (Vec<String>, usize) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
    assert!(
        fd >= 0,
        "open directory failed: {}",
        std::io::Error::last_os_error()
    );

    // 128 bytes fits ~1 entry, forcing many getdents64 calls.
    let mut buf = [0u8; 128];
    let mut names = Vec::new();
    let mut calls = 0usize;

    loop {
        let n =
            unsafe { libc::syscall(libc::SYS_getdents64, fd, buf.as_mut_ptr(), buf.len() as u32) };
        assert!(
            n >= 0,
            "getdents64 failed: {}",
            std::io::Error::last_os_error()
        );
        if n == 0 {
            break;
        }
        calls += 1;
        let mut offset = 0usize;
        while offset < n as usize {
            // struct linux_dirent64: d_ino(8), d_off(8), d_reclen(2), d_type(1), d_name(...)
            let reclen = u16::from_ne_bytes([buf[offset + 16], buf[offset + 17]]) as usize;
            let name_ptr = &buf[offset + 19..offset + reclen];
            let name_end = name_ptr
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(name_ptr.len());
            let name = std::str::from_utf8(&name_ptr[..name_end])
                .unwrap()
                .to_string();
            if name != "." && name != ".." {
                names.push(name);
            }
            offset += reclen;
        }
    }

    unsafe { libc::close(fd) };
    (names, calls)
}

/// Base-only directory: exercises the fast path (no dirents).
/// With a small getdents64 buffer, this makes many kernel re-entries.
#[test]
fn readdir_small_buf_base_only() {
    let s = YoloSession::new().expect("session setup");

    let dir = s.mnt_path("basedir");
    // Create files in the base layer by writing to the host path.
    let host_dir = s.base_path("basedir");
    fs::create_dir(&host_dir).expect("mkdir host");
    let mut expected = BTreeSet::new();
    for i in 0..30 {
        let name = format!("f-{i:03}.txt");
        fs::write(host_dir.join(&name), "x").expect("write");
        expected.insert(name);
    }

    let (names, calls) = readdir_small_buf(&dir);
    assert!(calls > 1, "expected multiple getdents64 calls, got {calls}");
    assert_eq!(
        names.len(),
        expected.len(),
        "base-only: duplicate or missing entries (got {} names for {} expected)",
        names.len(),
        expected.len()
    );
    let got: BTreeSet<String> = names.into_iter().collect();
    assert_eq!(got, expected, "base-only small-buf readdir mismatch");
}

/// Mixed directory: base files + staged creates + staged deletes.
/// Exercises the merge path with multiple getdents64 calls.
///
/// Fixed: tombstones no longer desync `off` and `ctx->pos` — the kernel
/// now syncs `off = ctx->pos` when no staged entries are emitted in
/// Phase 1, preventing double-skip of base entries on resumed getdents64.
#[test]
fn readdir_small_buf_mixed() {
    let s = YoloSession::new().expect("session setup");

    // Base layer: 20 files
    let host_dir = s.base_path("mixdir");
    fs::create_dir(&host_dir).expect("mkdir host");
    let mut expected = BTreeSet::new();
    for i in 0..20 {
        let name = format!("base-{i:03}.txt");
        fs::write(host_dir.join(&name), "x").expect("write base");
        expected.insert(name);
    }

    let dir = s.mnt_path("mixdir");

    // Stage: create 10 new files
    for i in 0..10 {
        let name = format!("new-{i:03}.txt");
        fs::write(dir.join(&name), "y").expect("write staged");
        expected.insert(name);
    }

    // Stage: delete 5 base files
    for i in 0..5 {
        let name = format!("base-{i:03}.txt");
        fs::remove_file(dir.join(&name)).expect("unlink");
        expected.remove(&name);
    }

    let (names, calls) = readdir_small_buf(&dir);
    assert!(calls > 1, "expected multiple getdents64 calls, got {calls}");
    assert_eq!(
        names.len(),
        expected.len(),
        "mixed: duplicate or missing entries (got {} names for {} expected)",
        names.len(),
        expected.len()
    );
    let got: BTreeSet<String> = names.into_iter().collect();
    assert_eq!(got, expected, "mixed small-buf readdir mismatch");
}

/// Rename + delete + create in one directory: readdir should emit only the
/// live staged names and skip the old base names hidden by tombstones.
#[test]
fn readdir_small_buf_rename_delete_create() {
    let s = YoloSession::new().expect("session setup");

    let host_dir = s.base_path("renmix");
    fs::create_dir(&host_dir).expect("mkdir host");
    let mut expected = BTreeSet::new();
    for i in 0..20 {
        let name = format!("base-{i:03}.txt");
        fs::write(host_dir.join(&name), "x").expect("write base");
        expected.insert(name);
    }

    let dir = s.mnt_path("renmix");

    fs::rename(dir.join("base-000.txt"), dir.join("renamed.txt")).expect("rename");
    expected.remove("base-000.txt");
    expected.insert("renamed.txt".to_string());

    fs::remove_file(dir.join("base-001.txt")).expect("unlink");
    expected.remove("base-001.txt");

    fs::write(dir.join("new.txt"), "y").expect("write staged");
    expected.insert("new.txt".to_string());

    let (names, calls) = readdir_small_buf(&dir);
    assert!(calls > 1, "expected multiple getdents64 calls, got {calls}");
    assert_eq!(
        names.len(),
        expected.len(),
        "rename/delete/create: duplicate or missing entries (got {} names for {} expected)",
        names.len(),
        expected.len()
    );
    let got: BTreeSet<String> = names.into_iter().collect();
    assert_eq!(
        got, expected,
        "rename/delete/create small-buf readdir mismatch"
    );
}

/// Staged-only directory: no base entries, all dirents.
/// Exercises the merge path where phase 2 has nothing to emit.
#[test]
fn readdir_small_buf_staged_only() {
    let s = YoloSession::new().expect("session setup");

    let dir = s.mnt_path("stagedir");
    fs::create_dir(&dir).expect("mkdir");

    let mut expected = BTreeSet::new();
    for i in 0..25 {
        let name = format!("s-{i:03}.txt");
        fs::write(dir.join(&name), "z").expect("write");
        expected.insert(name);
    }

    let (names, calls) = readdir_small_buf(&dir);
    assert!(calls > 1, "expected multiple getdents64 calls, got {calls}");
    assert_eq!(
        names.len(),
        expected.len(),
        "staged-only: duplicate or missing entries (got {} names for {} expected)",
        names.len(),
        expected.len()
    );
    let got: BTreeSet<String> = names.into_iter().collect();
    assert_eq!(got, expected, "staged-only small-buf readdir mismatch");
}

/// All-tombstone directory: base files all deleted, no staged creates.
/// Exercises the edge case where Phase 1 emits nothing (all entries are
/// tombstones) and the `off = ctx->pos` sync must kick in to avoid
/// double-skipping base entries in Phase 2.
#[test]
fn readdir_small_buf_all_tombstones() {
    let s = YoloSession::new().expect("session setup");

    // Base layer: 15 files.
    let host_dir = s.base_path("tombdir");
    fs::create_dir(&host_dir).expect("mkdir host");
    for i in 0..15 {
        fs::write(host_dir.join(format!("base-{i:03}.txt")), "x").expect("write base");
    }

    let dir = s.mnt_path("tombdir");

    // Delete all 15 base files through the mount (creates 15 tombstones).
    for i in 0..15 {
        fs::remove_file(dir.join(format!("base-{i:03}.txt"))).expect("unlink");
    }

    // readdir should return zero entries (all base entries overridden by
    // tombstones, no staged creates).
    let (names, _calls) = readdir_small_buf(&dir);
    assert!(
        names.is_empty(),
        "all-tombstone dir should be empty, got: {names:?}"
    );
}

/// Few staged entries + many base entries: the phase 1→2 boundary
/// lands in the middle of the getdents64 call sequence. This exercises
/// resumption into phase 2 after phase 1 has been fully emitted.
#[test]
fn readdir_small_buf_few_staged_many_base() {
    let s = YoloSession::new().expect("session setup");

    // Base layer: 28 files
    let host_dir = s.base_path("fewstage");
    fs::create_dir(&host_dir).expect("mkdir host");
    let mut expected = BTreeSet::new();
    for i in 0..28 {
        let name = format!("base-{i:03}.txt");
        fs::write(host_dir.join(&name), "x").expect("write base");
        expected.insert(name);
    }

    let dir = s.mnt_path("fewstage");

    // Stage: create only 2 new files (phase 1 is tiny)
    for i in 0..2 {
        let name = format!("new-{i}.txt");
        fs::write(dir.join(&name), "y").expect("write staged");
        expected.insert(name);
    }

    let (names, calls) = readdir_small_buf(&dir);
    assert!(calls > 1, "expected multiple getdents64 calls, got {calls}");
    assert_eq!(
        names.len(),
        expected.len(),
        "few-staged: duplicate or missing entries (got {} names for {} expected)",
        names.len(),
        expected.len()
    );
    let got: BTreeSet<String> = names.into_iter().collect();
    assert_eq!(got, expected, "few-staged small-buf readdir mismatch");
}
