//! Verify the kernel writes journal records in the expected wire format
//! (NUL-separated fields, including the stage `existed` bit).

use crate::helpers::YoloSession;
use std::fs;

/// Parse a raw journal line into (tag, fields) for format assertions.
fn parse_line(line: &[u8]) -> (u8, Vec<&[u8]>) {
    let fields: Vec<&[u8]> = line.split(|&b| b == 0).collect();
    (fields[0][0], fields)
}

/// Stage record format: S\0<path>\0<ino>\0<existed>\n (4 fields, no dtype).
/// The 4th field is the existed bit: 0 for a fresh create, 1 for copy-up of an
/// existing file.
#[test]
fn stage_record_format() {
    let s = YoloSession::new().expect("session setup");

    // A brand-new file → existed = 0.
    fs::write(s.mnt_path("test.txt"), "content").expect("create file");
    // Overwriting a seeded base file → copy-up → existed = 1.
    fs::write(s.mnt_path("hello.txt"), "changed").expect("modify base file");

    let journal_bytes = fs::read(s.root.join(".yolofs/journal")).expect("read journal");
    let stage_lines: Vec<&[u8]> = journal_bytes
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty() && line[0] == b'S')
        .collect();

    assert!(
        !stage_lines.is_empty(),
        "journal should contain an S record"
    );
    for line in &stage_lines {
        let (tag, fields) = parse_line(line);
        let shown: Vec<_> = fields.iter().map(|f| String::from_utf8_lossy(f)).collect();
        assert_eq!(tag, b'S');
        assert_eq!(
            fields.len(),
            4,
            "S record should be (S, path, ino, existed), got {}: {:?}",
            fields.len(),
            shown
        );
        assert!(
            fields[3] == b"0" || fields[3] == b"1",
            "existed field should be 0/1, got {:?}",
            shown[3]
        );
    }

    // The new file records existed=0; the overwritten base file records 1.
    let existed_for = |suffix: &str| -> Option<&[u8]> {
        stage_lines.iter().find_map(|line| {
            let fields: Vec<&[u8]> = line.split(|&b| b == 0).collect();
            (fields.len() == 4 && fields[1].ends_with(suffix.as_bytes())).then_some(fields[3])
        })
    };
    assert_eq!(existed_for("/test.txt"), Some(&b"0"[..]), "new file → existed 0");
    assert_eq!(
        existed_for("/hello.txt"),
        Some(&b"1"[..]),
        "overwritten base file → existed 1"
    );
}

/// Delete record format: D\0<path>\n (exactly 2 fields, no dtype).
#[test]
fn delete_record_has_no_dtype() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("to_delete.txt"), "bye").expect("create");
    fs::remove_file(s.mnt_path("to_delete.txt")).expect("delete");

    let journal_bytes = fs::read(s.root.join(".yolofs/journal")).expect("read journal");
    let delete_lines: Vec<&[u8]> = journal_bytes
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty() && line[0] == b'D')
        .collect();

    assert!(
        !delete_lines.is_empty(),
        "journal should contain a D record"
    );
    for line in &delete_lines {
        let (tag, fields) = parse_line(line);
        assert_eq!(tag, b'D');
        assert_eq!(
            fields.len(),
            2,
            "D record should have exactly 2 fields (D, path), got {}: {:?}",
            fields.len(),
            fields
                .iter()
                .map(|f| String::from_utf8_lossy(f))
                .collect::<Vec<_>>()
        );
    }
}

/// Rename record format: R\0<dst>\0<src>\n (exactly 3 fields, no dtype).
#[test]
fn rename_record_has_no_dtype() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("orig.txt"), "data").expect("create");
    fs::rename(s.mnt_path("orig.txt"), s.mnt_path("moved.txt")).expect("rename");

    let journal_bytes = fs::read(s.root.join(".yolofs/journal")).expect("read journal");
    let rename_lines: Vec<&[u8]> = journal_bytes
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty() && line[0] == b'R')
        .collect();

    assert!(
        !rename_lines.is_empty(),
        "journal should contain an R record"
    );
    for line in &rename_lines {
        let (tag, fields) = parse_line(line);
        assert_eq!(tag, b'R');
        assert_eq!(
            fields.len(),
            3,
            "R record should have exactly 3 fields (R, dst, src), got {}: {:?}",
            fields.len(),
            fields
                .iter()
                .map(|f| String::from_utf8_lossy(f))
                .collect::<Vec<_>>()
        );
    }
}

/// The `existed` field (4th of an S record) for the S record whose path ends
/// with `suffix`, read straight from the raw journal.
fn stage_existed(root: &std::path::Path, suffix: &str) -> Option<Vec<u8>> {
    let journal = fs::read(root.join(".yolofs/journal")).expect("read journal");
    journal
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty() && l[0] == b'S')
        .find_map(|l| {
            let f: Vec<&[u8]> = l.split(|&b| b == 0).collect();
            (f.len() == 4 && f[1].ends_with(suffix.as_bytes())).then(|| f[3].to_vec())
        })
}

/// A file modified under a RENAMED base directory copies up its real backing
/// (subdir/deep.txt) — the redirect is resolved at copy-up — so its stage
/// records existed=1. This is the motivating case for the existed bit.
#[test]
fn copy_up_under_renamed_dir_records_existed() {
    let s = YoloSession::new().expect("session setup");

    fs::rename(s.mnt_path("subdir"), s.mnt_path("moved")).expect("rename dir");
    fs::write(s.mnt_path("moved/deep.txt"), "nested\nextra\n").expect("modify child");

    assert_eq!(
        stage_existed(&s.root, "/moved/deep.txt").as_deref(),
        Some(&b"1"[..]),
        "child of a renamed dir copies up its base backing → existed 1"
    );
}

/// mkdir and symlink create fresh nodes — nothing existed before — so their
/// stage records carry existed=0.
#[test]
fn create_dir_and_symlink_record_not_existed() {
    let s = YoloSession::new().expect("session setup");

    fs::create_dir(s.mnt_path("newdir")).expect("mkdir");
    std::os::unix::fs::symlink("target", s.mnt_path("link")).expect("symlink");

    assert_eq!(
        stage_existed(&s.root, "/newdir").as_deref(),
        Some(&b"0"[..]),
        "mkdir creates a new node → existed 0"
    );
    assert_eq!(
        stage_existed(&s.root, "/link").as_deref(),
        Some(&b"0"[..]),
        "symlink creates a new node → existed 0"
    );
}
