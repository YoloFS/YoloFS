// yolo CLI — journal/parse.rs
//
// Parse the append-only journal file.
//
// Record format (NUL-separated fields, newline-terminated):
//   S\0<path>\0<ino>\0<pre>\n          — Stage (post = StagedFile(ino))
//   D\0<path>\0<pre>\n                 — Delete (post = Absence)
//   R\0<dst>\0<src>\0<src_pre>\0<dst_pre>\n  — Rename
//   P\0<gen>\0<name>\n                — Snapshot
//   T\0<gen>\0<target_gen>\n          — Travel
//   A\0<path>\0<op>\0<decision>\n      — Ask resolved (observational)
//   B\0<path>\0<op>\n                  — Blocked by a rule (observational)
//   (op = r/w; decision = y/d — allow/deny)
//
// Each *pre field is a tagged pre-op target: `A` (Absence), `I:<ino>`
// (StagedFile), `P:<abs-path>` (BasePath). It is parsed into a `Target` once
// here; a malformed tag or `I:` value skips the whole record.

use super::types::*;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

fn field_str(field: &[u8]) -> String {
    String::from_utf8_lossy(field).into_owned()
}

/// Parse a tagged pre-op `pre` field into a `Target`. Returns `None` for an
/// unknown tag or a malformed `I:` value, which skips the enclosing record.
fn parse_target(field: &[u8]) -> Option<Target> {
    match field {
        b"A" => Some(Target::Absence),
        _ => match field.split_first() {
            Some((b'I', rest)) if rest.first() == Some(&b':') => {
                String::from_utf8_lossy(&rest[1..])
                    .parse::<u32>()
                    .ok()
                    .map(Target::StagedFile)
            }
            Some((b'P', rest)) if rest.first() == Some(&b':') => {
                Some(Target::BasePath(field_str(&rest[1..])))
            }
            _ => None,
        },
    }
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
                // 4th field is the tagged pre-op backing; the post is
                // StagedFile(ino). A bad ino or pre tag skips the record.
                if let (Ok(ino), Some(pre)) = (ino_str.parse::<u32>(), parse_target(fields[3])) {
                    records.push(Record::Action(Action::Stage { path, ino, pre }));
                }
            }
            b"D" if fields.len() >= 3 => {
                let path = field_str(fields[1]);
                // 3rd field is the tagged pre-op backing; the post is Absence.
                if let Some(pre) = parse_target(fields[2]) {
                    records.push(Record::Action(Action::Delete { path, pre }));
                }
            }
            b"R" if fields.len() >= 5 => {
                let dst = field_str(fields[1]);
                let src = field_str(fields[2]);
                // 4th/5th fields are the source/destination pre-op backings.
                if let (Some(src_pre), Some(dst_pre)) =
                    (parse_target(fields[3]), parse_target(fields[4]))
                {
                    records.push(Record::Action(Action::Rename {
                        src,
                        dst,
                        src_pre,
                        dst_pre,
                    }));
                }
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
                let decision = (fields[3].len() == 1)
                    .then(|| fields[3][0])
                    .and_then(crate::perm::Decision::from_letter);
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
        let records = parse(b"S\0/a\01\0A\nD\0/b\0A\nR\0/d\0/c\0A\0A\n").unwrap();
        assert_eq!(records.len(), 3);
        assert!(
            matches!(&records[0], Record::Action(Action::Stage { path, ino: 1, .. }) if path == "/a")
        );
        assert!(matches!(&records[1], Record::Action(Action::Delete { path, .. }) if path == "/b"));
        assert!(
            matches!(&records[2], Record::Action(Action::Rename { dst, src, .. }) if dst == "/d" && src == "/c")
        );
    }

    #[test]
    fn parse_target_tags() {
        assert_eq!(parse_target(b"A"), Some(Target::Absence));
        assert_eq!(parse_target(b"I:7"), Some(Target::StagedFile(7)));
        assert_eq!(
            parse_target(b"P:/base/f"),
            Some(Target::BasePath("/base/f".into()))
        );
        // Malformed: bad ino, unknown tag, missing colon, empty.
        assert_eq!(parse_target(b"I:x"), None);
        assert_eq!(parse_target(b"Z:/f"), None);
        assert_eq!(parse_target(b"I"), None);
        assert_eq!(parse_target(b""), None);
    }

    #[test]
    fn parse_stage_and_delete_pre() {
        // S: 4th field is the tagged pre-op target.
        let r = parse(b"S\0/a\01\0P:/a\n").unwrap();
        assert!(matches!(
            &r[0],
            Record::Action(Action::Stage { pre: Target::BasePath(p), .. }) if p == "/a"
        ));
        let r = parse(b"S\0/a\01\0A\n").unwrap();
        assert!(matches!(
            &r[0],
            Record::Action(Action::Stage {
                pre: Target::Absence,
                ..
            })
        ));
        let r = parse(b"S\0/a\02\0I:9\n").unwrap();
        assert!(matches!(
            &r[0],
            Record::Action(Action::Stage {
                pre: Target::StagedFile(9),
                ..
            })
        ));
        // D: 3rd field is the tagged pre-op target.
        let r = parse(b"D\0/a\0P:/a\n").unwrap();
        assert!(matches!(
            &r[0],
            Record::Action(Action::Delete { pre: Target::BasePath(p), .. }) if p == "/a"
        ));
        // Malformed pre tag skips the record.
        let r = parse(b"S\0/a\01\0Z:/x\nD\0/b\0bad\n").unwrap();
        assert!(r.is_empty(), "records with malformed pre skipped: {r:?}");
    }

    #[test]
    fn parse_rename_pres_roundtrip() {
        let r = parse(b"R\0/dst\0/src\0P:/base/src\0I:4\n").unwrap();
        assert!(matches!(
            &r[0],
            Record::Action(Action::Rename { dst, src, src_pre: Target::BasePath(sp), dst_pre: Target::StagedFile(4) })
                if dst == "/dst" && src == "/src" && sp == "/base/src"
        ));
    }

    #[test]
    fn parse_snapshot_record() {
        let records = parse(b"S\0/a\01\0A\nP\01\0build\nS\0/a\02\0A\n").unwrap();
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
        let records = parse(b"S\0/src/main.rs\01\0A\n").unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            matches!(&records[0], Record::Action(Action::Stage { path, ino: 1, .. }) if path == "/src/main.rs")
        );
    }

    // ── Malformed record tests ─────────────────────────────────────────

    #[test]
    fn malformed_s_record_too_few_fields_skipped() {
        // S record missing fields (needs path, ino, preimage) — should be skipped
        let records = parse(b"S\0/file\nS\0/good\02\0A\n").unwrap();
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
        let records = parse(b"D\nS\0/good\01\0A\n").unwrap();
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
        // R record with only dst+src (needs src_pre+dst_pre too) — skipped.
        let records = parse(b"R\0/dst\0/src\0A\nS\0/good\01\0A\n").unwrap();
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
        let records = parse(b"D\0/foo\0A\n").unwrap();
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
            Record::Note(Note::Ask { path, op: Op::Read, decision: crate::perm::Decision::Deny }) if path == "/etc/hosts"
        ));
    }

    #[test]
    fn parse_ask_record_rejects_rule_only_decisions() {
        let records =
            parse(b"A\0/ask\0r\0a\nA\0/write-ask\0r\0w\nA\0/read-only\0r\0r\nA\0/hide\0r\0h\n")
                .unwrap();
        assert!(
            records.is_empty(),
            "rule modes must not parse as ask decisions: {records:?}"
        );
    }

    #[test]
    fn parse_ask_record_rejects_multibyte_decisions() {
        let records = parse(b"A\0/yes\0r\0yes\nA\0/deny\0r\0deny\n").unwrap();
        assert!(
            records.is_empty(),
            "multi-byte decisions must not parse as ask decisions: {records:?}"
        );
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
        let records = parse(b"S\0/a\01\0A\nB\0/etc/passwd\0w\nD\0/a\0A\n").unwrap();
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
        let records = parse(b"B\nS\0/good\01\0A\n").unwrap();
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
