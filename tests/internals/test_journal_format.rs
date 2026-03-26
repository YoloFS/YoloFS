//! Verify the kernel writes journal records in the expected wire format
//! (without dtype fields).

use crate::helpers::AgfsSession;
use std::fs;

/// Parse a raw journal line into (tag, fields) for format assertions.
fn parse_line(line: &[u8]) -> (u8, Vec<&[u8]>) {
    let fields: Vec<&[u8]> = line.split(|&b| b == 0).collect();
    (fields[0][0], fields)
}

/// Stage record format: S\0<path>\0<ino>\n (exactly 3 fields, no dtype).
#[test]
fn stage_record_has_no_dtype() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("test.txt"), "content").expect("create file");

    let journal_bytes = fs::read(s.root.join(".agfs/journal")).expect("read journal");
    let stage_lines: Vec<&[u8]> = journal_bytes
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty() && line[0] == b'S')
        .collect();

    assert!(!stage_lines.is_empty(), "journal should contain an S record");
    for line in &stage_lines {
        let (tag, fields) = parse_line(line);
        assert_eq!(tag, b'S');
        assert_eq!(
            fields.len(),
            3,
            "S record should have exactly 3 fields (S, path, ino), got {}: {:?}",
            fields.len(),
            fields.iter().map(|f| String::from_utf8_lossy(f)).collect::<Vec<_>>()
        );
    }
}

/// Delete record format: D\0<path>\n (exactly 2 fields, no dtype).
#[test]
fn delete_record_has_no_dtype() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("to_delete.txt"), "bye").expect("create");
    fs::remove_file(s.mnt_path("to_delete.txt")).expect("delete");

    let journal_bytes = fs::read(s.root.join(".agfs/journal")).expect("read journal");
    let delete_lines: Vec<&[u8]> = journal_bytes
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty() && line[0] == b'D')
        .collect();

    assert!(!delete_lines.is_empty(), "journal should contain a D record");
    for line in &delete_lines {
        let (tag, fields) = parse_line(line);
        assert_eq!(tag, b'D');
        assert_eq!(
            fields.len(),
            2,
            "D record should have exactly 2 fields (D, path), got {}: {:?}",
            fields.len(),
            fields.iter().map(|f| String::from_utf8_lossy(f)).collect::<Vec<_>>()
        );
    }
}

/// Rename record format: R\0<dst>\0<src>\n (exactly 3 fields, no dtype).
#[test]
fn rename_record_has_no_dtype() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("orig.txt"), "data").expect("create");
    fs::rename(s.mnt_path("orig.txt"), s.mnt_path("moved.txt")).expect("rename");

    let journal_bytes = fs::read(s.root.join(".agfs/journal")).expect("read journal");
    let rename_lines: Vec<&[u8]> = journal_bytes
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty() && line[0] == b'R')
        .collect();

    assert!(!rename_lines.is_empty(), "journal should contain an R record");
    for line in &rename_lines {
        let (tag, fields) = parse_line(line);
        assert_eq!(tag, b'R');
        assert_eq!(
            fields.len(),
            3,
            "R record should have exactly 3 fields (R, dst, src), got {}: {:?}",
            fields.len(),
            fields.iter().map(|f| String::from_utf8_lossy(f)).collect::<Vec<_>>()
        );
    }
}
