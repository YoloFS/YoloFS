// yolo CLI — journal/parse.rs
//
// Parse the append-only journal file.
//
// Record format (NUL-separated fields, newline-terminated):
//   S\0<path>\0<ino>\n                — Stage (staged content at path)
//   D\0<path>\n                       — Delete
//   R\0<dst>\0<src>\n                  — Rename
//   M\0<gen>\0<name>\n                — Mark
//   J\0<gen>\0<target_gen>\n          — Jump
//   B\0<path>\n                       — Blocked (permission denied;
//                                       observational, no state change)

use super::types::*;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

fn field_str(field: &[u8]) -> String {
    String::from_utf8_lossy(field).into_owned()
}

/// Read and parse the journal file.
pub(super) fn read(yolo_dir: &Path) -> Result<Vec<Record>> {
    let journal_path = yolo_dir.join("journal");
    if !journal_path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read(&journal_path).context("reading journal file")?;
    parse(&data)
}

/// Parse journal bytes into records.
pub(super) fn parse(data: &[u8]) -> Result<Vec<Record>> {
    let mut records = Vec::new();

    for line in data.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&[u8]> = line.split(|&b| b == 0).collect();
        if fields.is_empty() {
            continue;
        }
        let tag = fields[0];
        match tag {
            b"S" if fields.len() >= 3 => {
                let path = field_str(fields[1]);
                let ino_str = String::from_utf8_lossy(fields[2]);

                if let Ok(ino) = ino_str.parse::<u32>() {
                    records.push(Record::Action(Action::Stage { path, ino }));
                }
            }
            b"D" if fields.len() >= 2 => {
                let path = field_str(fields[1]);
                records.push(Record::Action(Action::Delete { path }));
            }
            b"R" if fields.len() >= 3 => {
                let dst = field_str(fields[1]);
                let src = field_str(fields[2]);

                records.push(Record::Action(Action::Rename { src, dst }));
            }
            b"M" if fields.len() >= 3 => {
                let gen_str = String::from_utf8_lossy(fields[1]);
                let name = field_str(fields[2]);
                if let Ok(gen_id) = gen_str.parse::<u64>() {
                    records.push(Record::Meta(Meta::Mark { gen_id, name }));
                }
            }
            b"J" if fields.len() >= 3 => {
                let gen_str = String::from_utf8_lossy(fields[1]);
                let target_str = String::from_utf8_lossy(fields[2]);
                if let (Ok(gen_id), Ok(target_gen)) =
                    (gen_str.parse::<u64>(), target_str.parse::<u64>())
                {
                    records.push(Record::Meta(Meta::Jump { gen_id, target_gen }));
                }
            }
            b"B" if fields.len() >= 2 => {
                let path = field_str(fields[1]);
                records.push(Record::Note(Note::Block { path }));
            }
            _ => {}
        }
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_missing_journal_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let records = read(dir.path()).unwrap();
        assert!(records.is_empty());
    }

    // ── Parse tests (pure in-memory) ───────────────────────────────────

    #[test]
    fn parse_multiple() {
        let records = parse(b"S\0/a\01\nD\0/b\nR\0/d\0/c\n").unwrap();
        assert_eq!(records.len(), 3);
        assert!(
            matches!(&records[0], Record::Action(Action::Stage { path, ino: 1 }) if path == "/a")
        );
        assert!(matches!(&records[1], Record::Action(Action::Delete { path }) if path == "/b"));
        assert!(
            matches!(&records[2], Record::Action(Action::Rename { dst, src }) if dst == "/d" && src == "/c")
        );
    }

    #[test]
    fn parse_mark_record() {
        let records = parse(b"S\0/a\01\nM\01\0build\nS\0/a\02\n").unwrap();
        assert_eq!(records.len(), 3);
        assert!(
            matches!(&records[1], Record::Meta(Meta::Mark { gen_id, name }) if *gen_id == 1 && name == "build")
        );
    }

    #[test]
    fn parse_jump_record() {
        let records = parse(b"J\x004\x002\n").unwrap();
        assert_eq!(records.len(), 1);
        match &records[0] {
            Record::Meta(Meta::Jump { gen_id, target_gen }) => {
                assert_eq!(*gen_id, 4);
                assert_eq!(*target_gen, 2);
            }
            _ => panic!("expected Jump record"),
        }
    }

    // ── Path tests ────────────────────────────────────────────────────

    #[test]
    fn parse_entry_full_path() {
        let records = parse(b"S\0/src/main.rs\01\n").unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            matches!(&records[0], Record::Action(Action::Stage { path, ino: 1 }) if path == "/src/main.rs")
        );
    }

    // ── Malformed record tests ─────────────────────────────────────────

    #[test]
    fn malformed_s_record_too_few_fields_skipped() {
        // S record with only 1 field (needs path + ino) — should be skipped
        let records = parse(b"S\0/file\nS\0/good\02\n").unwrap();
        assert_eq!(
            records.len(),
            1,
            "malformed record should be skipped: {:?}",
            records
        );
        assert!(matches!(
            &records[0],
            Record::Action(Action::Stage { path, ino: 2, .. }) if path == "/good"
        ));
    }

    #[test]
    fn malformed_d_record_too_few_fields_skipped() {
        // D record with only tag (needs path) — should be skipped
        let records = parse(b"D\nS\0/good\01\n").unwrap();
        assert_eq!(
            records.len(),
            1,
            "malformed D record should be skipped: {:?}",
            records
        );
        assert!(matches!(
            &records[0],
            Record::Action(Action::Stage { path, ino: 1, .. }) if path == "/good"
        ));
    }

    #[test]
    fn malformed_r_record_too_few_fields_skipped() {
        // R record with only 2 fields (needs dst + src) — should be skipped
        let records = parse(b"R\0/file\nS\0/good\01\n").unwrap();
        assert_eq!(
            records.len(),
            1,
            "malformed R record should be skipped: {:?}",
            records
        );
        assert!(matches!(
            &records[0],
            Record::Action(Action::Stage { path, ino: 1, .. }) if path == "/good"
        ));
    }

    #[test]
    fn parse_delete() {
        let records = parse(b"D\0/foo\n").unwrap();
        assert_eq!(records.len(), 1);
        assert!(matches!(&records[0], Record::Action(Action::Delete { path }) if path == "/foo"));
    }

    // ── Block (B) record tests ─────────────────────────────────────────

    #[test]
    fn parse_block_record() {
        let records = parse(b"B\0/etc/passwd\n").unwrap();
        assert_eq!(records.len(), 1);
        assert!(matches!(
            &records[0],
            Record::Note(Note::Block { path }) if path == "/etc/passwd"
        ));
    }

    #[test]
    fn parse_block_interleaved_with_actions() {
        // B records ride alongside S/D/R within a segment.
        let records = parse(b"S\0/a\01\nB\0/etc/passwd\nD\0/a\n").unwrap();
        assert_eq!(records.len(), 3);
        assert!(matches!(
            &records[0],
            Record::Action(Action::Stage { path, ino: 1 }) if path == "/a"
        ));
        assert!(matches!(
            &records[1],
            Record::Note(Note::Block { path }) if path == "/etc/passwd"
        ));
        assert!(matches!(
            &records[2],
            Record::Action(Action::Delete { path }) if path == "/a"
        ));
    }

    #[test]
    fn malformed_b_record_too_few_fields_skipped() {
        // B record with only the tag (needs path) — should be skipped.
        let records = parse(b"B\nS\0/good\01\n").unwrap();
        assert_eq!(
            records.len(),
            1,
            "malformed B record should be skipped: {:?}",
            records
        );
        assert!(matches!(
            &records[0],
            Record::Action(Action::Stage { path, ino: 1, .. }) if path == "/good"
        ));
    }
}
