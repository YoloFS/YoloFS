//! Verify the kernel writes journal records in the expected wire format
//! (NUL-separated fields) with tagged pre-op targets: `a` (None),
//! `s:<ino>` (StagedFile), `b:<abs-path>` (BasePath).

use crate::helpers::YoloSession;
use std::fs;

/// All records with the given tag, each split into its NUL-separated fields.
fn records(root: &std::path::Path, tag: u8) -> Vec<Vec<Vec<u8>>> {
    let journal = fs::read(root.join(".yolofs/journal")).expect("read journal");
    journal
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty() && l[0] == tag)
        .map(|l| l.split(|&b| b == 0).map(|f| f.to_vec()).collect())
        .collect()
}

/// The pre field of the latest record with `tag` whose path field (`path_idx`)
/// ends with `suffix`.
fn pre_of(
    root: &std::path::Path,
    tag: u8,
    path_idx: usize,
    pre_idx: usize,
    suffix: &str,
) -> Vec<u8> {
    records(root, tag)
        .into_iter()
        .rev()
        .find(|f| {
            f.get(path_idx)
                .is_some_and(|p| p.ends_with(suffix.as_bytes()))
        })
        .and_then(|f| f.into_iter().nth(pre_idx))
        .unwrap_or_else(|| panic!("no {} record for {suffix}", tag as char))
}

/// Stage record: S\0<path>\0<ino>\0<pre>\n (4 fields). A fresh create carries
/// `a`; overwriting a seeded base file carries `b:<base>`.
#[test]
fn stage_record_format() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("test.txt"), "content").expect("create file");
    fs::write(s.mnt_path("hello.txt"), "changed").expect("modify base file");

    for f in records(&s.root, b'S') {
        assert_eq!(f.len(), 4, "S record should be (S, path, ino, pre): {f:?}");
    }
    assert_eq!(
        pre_of(&s.root, b'S', 1, 3, "/test.txt"),
        b"a",
        "fresh create → a"
    );
    let pre = pre_of(&s.root, b'S', 1, 3, "/hello.txt");
    assert!(
        pre.starts_with(b"b:") && pre.ends_with(b"hello.txt"),
        "overwritten base file → b:<base>, got {:?}",
        String::from_utf8_lossy(&pre)
    );
}

/// Delete record: D\0<path>\0<pre>\n (3 fields). Deleting a seeded base file
/// carries `b:<base>`.
#[test]
fn delete_record_format() {
    let s = YoloSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).expect("delete base file");

    for f in records(&s.root, b'D') {
        assert_eq!(f.len(), 3, "D record should be (D, path, pre): {f:?}");
    }
    let pre = pre_of(&s.root, b'D', 1, 2, "/hello.txt");
    assert!(
        pre.starts_with(b"b:") && pre.ends_with(b"hello.txt"),
        "deleted base file → b:<base>, got {:?}",
        String::from_utf8_lossy(&pre)
    );
}

/// Rename record: R\0<dst>\0<src>\0<src_pre>\0<dst_pre>\n (5 fields). Renaming a
/// staged file onto a fresh name carries `s:<ino>` src_pre and `a` dst_pre.
#[test]
fn rename_record_format_staged_source() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("orig.txt"), "data").expect("create");
    fs::rename(s.mnt_path("orig.txt"), s.mnt_path("moved.txt")).expect("rename");

    let rec = records(&s.root, b'R')
        .into_iter()
        .find(|f| f.get(1).is_some_and(|d| d.ends_with(b"/moved.txt")))
        .expect("R record for the rename");
    assert_eq!(
        rec.len(),
        5,
        "R should be (R, dst, src, src_pre, dst_pre): {rec:?}"
    );
    assert!(
        rec[3].starts_with(b"s:"),
        "staged source → s:<ino> src_pre, got {:?}",
        String::from_utf8_lossy(&rec[3])
    );
    assert_eq!(rec[4], b"a", "fresh destination → a dst_pre");
}

/// Renaming a seeded base file carries `b:<base>` src_pre and `a` dst_pre.
#[test]
fn rename_record_base_source() {
    let s = YoloSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("hello2.txt")).expect("rename base file");

    let rec = records(&s.root, b'R')
        .into_iter()
        .find(|f| f.get(1).is_some_and(|d| d.ends_with(b"/hello2.txt")))
        .expect("R record");
    assert!(
        rec[3].starts_with(b"b:") && rec[3].ends_with(b"hello.txt"),
        "base source → b:<base> src_pre, got {:?}",
        String::from_utf8_lossy(&rec[3])
    );
    assert_eq!(rec[4], b"a", "fresh destination → a dst_pre");
}

/// Renaming onto an existing base file records that file as the `b:<base>`
/// dst_pre (the clobbered destination).
#[test]
fn rename_onto_existing_base_records_dst_pre() {
    let s = YoloSession::new().expect("session setup");

    // Both `hello.txt` and `subdir/deep.txt` are seeded base files.
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("subdir/deep.txt")).expect("rename onto base");

    let rec = records(&s.root, b'R')
        .into_iter()
        .find(|f| f.get(1).is_some_and(|d| d.ends_with(b"/subdir/deep.txt")))
        .expect("R record");
    assert!(
        rec[4].starts_with(b"b:") && rec[4].ends_with(b"deep.txt"),
        "clobbered base destination → b:<base> dst_pre, got {:?}",
        String::from_utf8_lossy(&rec[4])
    );
}

/// A file modified under a RENAMED base directory resolves the redirect at
/// copy-up, so its stage pre points at the real backing as `b:<.../subdir/deep.txt>`.
#[test]
fn copy_up_under_renamed_dir_records_backing_pre() {
    let s = YoloSession::new().expect("session setup");

    fs::rename(s.mnt_path("subdir"), s.mnt_path("moved")).expect("rename dir");
    fs::write(s.mnt_path("moved/deep.txt"), "nested\nextra\n").expect("modify child");

    let pre = pre_of(&s.root, b'S', 1, 3, "/moved/deep.txt");
    assert!(
        pre.starts_with(b"b:") && pre.ends_with(b"subdir/deep.txt"),
        "pre should point at the real backing b:<subdir/deep.txt>, got {:?}",
        String::from_utf8_lossy(&pre)
    );
}

/// Re-staging an already-staged file across a snapshot copies up from the prior
/// snapshot's inode, so the re-stage pre is `s:<ino>`, not the base.
#[test]
fn restage_after_snapshot_records_inode_pre() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("first stage");
    s.cli(&["snapshot", "s1"]).expect("snapshot");
    fs::write(s.mnt_path("hello.txt"), "v2\n").expect("re-stage after snapshot");

    let pre = pre_of(&s.root, b'S', 1, 3, "/hello.txt");
    assert!(
        pre.starts_with(b"s:"),
        "re-COW of a staged file → s:<ino> pre, got {:?}",
        String::from_utf8_lossy(&pre)
    );
}

/// mkdir and symlink create fresh nodes — nothing existed — so their stage
/// records carry `a`.
#[test]
fn create_dir_and_symlink_have_absent_pre() {
    let s = YoloSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("newdir")).expect("mkdir");
    std::os::unix::fs::symlink("target", s.mnt_path("link")).expect("symlink");

    assert_eq!(pre_of(&s.root, b'S', 1, 3, "/newdir"), b"a", "mkdir → a");
    assert_eq!(pre_of(&s.root, b'S', 1, 3, "/link"), b"a", "symlink → a");
}
