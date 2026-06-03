// yolo CLI — journal/parse.rs
//
// Parse the append-only journal file.
//
// Record format (NUL-separated fields, newline-terminated):
//   S\0<path>\0<ino>\0<preimage>\n     — Stage (preimage = absolute path of the
//                                       overwritten content, empty for a create)
//   D\0<path>\0<preimage>\n            — Delete (preimage = absolute path of the
//                                       removed content, empty for a no-op)
//   R\0<dst>\0<src>\n                  — Rename
//   P\0<gen>\0<name>\n                — Snapshot
//   T\0<gen>\0<target_gen>\n          — Travel
//   A\0<path>\0<op>\0<decision>\n      — Ask resolved (observational)
//   B\0<path>\0<op>\n                  — Blocked by a rule (observational)
//   (op = r/w; decision = a/y/r/d/h — ask/allow/read/deny/hide)

use super::types::*;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

fn field_str(field: &[u8]) -> String {
    String::from_utf8_lossy(field).into_owned()
}

/// A pre-image field: the absolute path of the old content, or `None` when
/// empty (a fresh create / no-op delete — nothing existed to point at).
fn preimage_field(field: &[u8]) -> Option<String> {
    (!field.is_empty()).then(|| field_str(field))
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
            b"S" if fields.len() >= 4 => {
                let path = field_str(fields[1]);
                let ino_str = String::from_utf8_lossy(fields[2]);
                // 4th field is the pre-image: the absolute path of the content
                // this stage overwrote, empty for a fresh create. The kernel
                // always writes it, so the field is required.
                let preimage = preimage_field(fields[3]);

                if let Ok(ino) = ino_str.parse::<u32>() {
                    records.push(Record::Action(Action::Stage {
                        path,
                        ino,
                        preimage,
                    }));
                }
            }
            b"D" if fields.len() >= 3 => {
                let path = field_str(fields[1]);
                // 3rd field is the pre-image (empty for a no-op delete).
                let preimage = preimage_field(fields[2]);
                records.push(Record::Action(Action::Delete { path, preimage }));
            }
            b"R" if fields.len() >= 3 => {
                let dst = field_str(fields[1]);
                let src = field_str(fields[2]);

                records.push(Record::Action(Action::Rename { src, dst }));
            }
            b"P" if fields.len() >= 3 => {
                let gen_str = String::from_utf8_lossy(fields[1]);
                let name = field_str(fields[2]);
                if let Ok(gen_id) = gen_str.parse::<u64>() {
                    records.push(Record::Marker(Marker::Snapshot { gen_id, name }));
                }
            }
            b"T" if fields.len() >= 3 => {
                let gen_str = String::from_utf8_lossy(fields[1]);
                let target_str = String::from_utf8_lossy(fields[2]);
                if let (Ok(gen_id), Ok(target_gen)) =
                    (gen_str.parse::<u64>(), target_str.parse::<u64>())
                {
                    records.push(Record::Marker(Marker::Travel { gen_id, target_gen }));
                }
            }
            b"A" if fields.len() >= 4 => {
                let path = field_str(fields[1]);
                let op = fields[2].first().and_then(|&b| Op::from_byte(b));
                let decision = fields[3]
                    .first()
                    .and_then(|&b| crate::perm::Perm::from_letter(b));
                if let (Some(op), Some(decision)) = (op, decision) {
                    records.push(Record::Note(Note::Ask { path, op, decision }));
                }
            }
            b"B" if fields.len() >= 3 => {
                let path = field_str(fields[1]);
                if let Some(op) = fields[2].first().and_then(|&b| Op::from_byte(b)) {
                    records.push(Record::Note(Note::Block { path, op }));
                }
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
        let records = parse(b"S\0/a\01\0\nD\0/b\0\nR\0/d\0/c\n").unwrap();
        assert_eq!(records.len(), 3);
        assert!(
            matches!(&records[0], Record::Action(Action::Stage { path, ino: 1, .. }) if path == "/a")
        );
        assert!(matches!(&records[1], Record::Action(Action::Delete { path, .. }) if path == "/b"));
        assert!(
            matches!(&records[2], Record::Action(Action::Rename { dst, src }) if dst == "/d" && src == "/c")
        );
    }

    #[test]
    fn parse_stage_and_delete_preimage() {
        // S: 4th field is the pre-image path → Some; empty → None (create).
        let r = parse(b"S\0/a\01\0/a\n").unwrap();
        assert!(matches!(
            &r[0],
            Record::Action(Action::Stage { preimage: Some(p), .. }) if p == "/a"
        ));
        let r = parse(b"S\0/a\01\0\n").unwrap();
        assert!(matches!(
            &r[0],
            Record::Action(Action::Stage { preimage: None, .. })
        ));
        // D: 3rd field is the pre-image path → Some; empty → None (no-op).
        let r = parse(b"D\0/a\0/a\n").unwrap();
        assert!(matches!(
            &r[0],
            Record::Action(Action::Delete { preimage: Some(p), .. }) if p == "/a"
        ));
        let r = parse(b"D\0/a\0\n").unwrap();
        assert!(matches!(
            &r[0],
            Record::Action(Action::Delete { preimage: None, .. })
        ));
    }

    #[test]
    fn parse_snapshot_record() {
        let records = parse(b"S\0/a\01\0\nP\01\0build\nS\0/a\02\0\n").unwrap();
        assert_eq!(records.len(), 3);
        assert!(
            matches!(&records[1], Record::Marker(Marker::Snapshot { gen_id, name }) if *gen_id == 1 && name == "build")
        );
    }

    #[test]
    fn parse_travel_record() {
        let records = parse(b"T\x004\x002\n").unwrap();
        assert_eq!(records.len(), 1);
        match &records[0] {
            Record::Marker(Marker::Travel { gen_id, target_gen }) => {
                assert_eq!(*gen_id, 4);
                assert_eq!(*target_gen, 2);
            }
            _ => panic!("expected Travel record"),
        }
    }

    // ── Path tests ────────────────────────────────────────────────────

    #[test]
    fn parse_entry_full_path() {
        let records = parse(b"S\0/src/main.rs\01\0\n").unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            matches!(&records[0], Record::Action(Action::Stage { path, ino: 1, .. }) if path == "/src/main.rs")
        );
    }

    // ── Malformed record tests ─────────────────────────────────────────

    #[test]
    fn malformed_s_record_too_few_fields_skipped() {
        // S record missing fields (needs path, ino, preimage) — should be skipped
        let records = parse(b"S\0/file\nS\0/good\02\0\n").unwrap();
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
        let records = parse(b"D\nS\0/good\01\0\n").unwrap();
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
        let records = parse(b"R\0/file\nS\0/good\01\0\n").unwrap();
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
        let records = parse(b"D\0/foo\0\n").unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            matches!(&records[0], Record::Action(Action::Delete { path, .. }) if path == "/foo")
        );
    }

    // ── Note (A/B) record tests ────────────────────────────────────────

    #[test]
    fn parse_ask_record() {
        // A\0<path>\0<op>\0<decision>  (op = r = read, decision = d = deny)
        let records = parse(b"A\0/etc/hosts\0r\0d\n").unwrap();
        assert_eq!(records.len(), 1);
        assert!(matches!(
            &records[0],
            Record::Note(Note::Ask { path, op: Op::Read, decision: crate::perm::Perm::Deny }) if path == "/etc/hosts"
        ));
    }

    #[test]
    fn parse_block_record() {
        // B\0<path>\0<op>  (op = w = write)
        let records = parse(b"B\0/etc/passwd\0w\n").unwrap();
        assert_eq!(records.len(), 1);
        assert!(matches!(
            &records[0],
            Record::Note(Note::Block { path, op: Op::Write }) if path == "/etc/passwd"
        ));
    }

    #[test]
    fn parse_block_interleaved_with_actions() {
        // B records ride alongside S/D/R within a segment.
        let records = parse(b"S\0/a\01\0\nB\0/etc/passwd\0w\nD\0/a\0\n").unwrap();
        assert_eq!(records.len(), 3);
        assert!(matches!(
            &records[0],
            Record::Action(Action::Stage { path, ino: 1, .. }) if path == "/a"
        ));
        assert!(matches!(
            &records[1],
            Record::Note(Note::Block { path, op: Op::Write }) if path == "/etc/passwd"
        ));
        assert!(matches!(
            &records[2],
            Record::Action(Action::Delete { path, .. }) if path == "/a"
        ));
    }

    #[test]
    fn malformed_b_record_too_few_fields_skipped() {
        // B record with only the tag (needs path) — should be skipped.
        let records = parse(b"B\nS\0/good\01\0\n").unwrap();
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
