// yolo CLI — journal/parse.rs
//
// Parse the append-only journal file.
//
// Record format (NUL-separated fields, newline-terminated):
//   S\0<path>\0<ino>\0<pre>\n          — Stage (post = StagedFile(ino))
//   D\0<path>\0<pre>\n                 — Delete (post = Absence)
//   R\0<dst>\0<src>\0<src_pre>\0<dst_pre>\n  — Rename
//   P\0<name>\n                       — Snapshot (gen = record's P/T position)
//   T\0<target_gen>\n                 — Travel  (gen = record's P/T position)
//   G\0<path>\0<op>\0<result>\n        — Gate result (observational)
//   C\0<path>\0<policy>\n           — Live policy configuration (observational)
//   (op = r/w; result = d/y/n; policy = q/a/w/r/d/h/u)
//
// Record tags are uppercase. Each *pre field is a tagged pre-op target whose
// tag is the lowercased first letter of the `Target` variant: `a` (Absence),
// `s:<ino>` (StagedFile), `b:<abs-path>` (BasePath). It is parsed into a
// `Target` once here; a malformed tag or `s:` value skips the whole record.

use super::types::*;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

fn field_str(field: &[u8]) -> String {
    String::from_utf8_lossy(field).into_owned()
}

/// Parse a tagged pre-op `pre` field into a `Target`. The tag is the lowercased
/// first letter of the variant (`a`/`s`/`b`). Returns `None` for an unknown tag
/// or a malformed `s:` value, which skips the enclosing record.
fn parse_target(field: &[u8]) -> Option<Target> {
    match field {
        b"a" => Some(Target::Absence),
        _ => match field.split_first() {
            Some((b's', rest)) if rest.first() == Some(&b':') => {
                String::from_utf8_lossy(&rest[1..])
                    .parse::<u32>()
                    .ok()
                    .map(Target::StagedFile)
            }
            Some((b'b', rest)) if rest.first() == Some(&b':') => {
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
            b"P" if fields.len() >= 2 => {
                // The marker's gen is its position in the P/T sequence, assigned
                // by `Journal::new`; only the name is on the wire.
                let name = field_str(fields[1]);
                records.push(Record::Marker(Marker::Snapshot { name }));
            }
            b"T" if fields.len() >= 2 => {
                let target_str = String::from_utf8_lossy(fields[1]);
                if let Ok(target_gen) = target_str.parse::<u64>() {
                    records.push(Record::Marker(Marker::Travel { target_gen }));
                }
            }
            b"G" if fields.len() == 4 => {
                let path = field_str(fields[1]);
                let op = (fields[2].len() == 1)
                    .then(|| fields[2][0])
                    .and_then(Op::from_byte);
                let result = (fields[3].len() == 1)
                    .then(|| fields[3][0])
                    .and_then(GateResult::from_byte);
                if let (Some(op), Some(result)) = (op, result) {
                    records.push(Record::Note(Note::Gate { path, op, result }));
                }
            }
            b"C" if fields.len() == 3 => {
                let path = field_str(fields[1]);
                let policy = (fields[2].len() == 1)
                    .then(|| fields[2][0])
                    .and_then(Policy::from_byte);
                if let Some(policy) = policy {
                    records.push(Record::Note(Note::Configure { path, policy }));
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
        let records = parse(b"S\0/a\01\0a\nD\0/b\0a\nR\0/d\0/c\0a\0a\n").unwrap();
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
        assert_eq!(parse_target(b"a"), Some(Target::Absence));
        assert_eq!(parse_target(b"s:7"), Some(Target::StagedFile(7)));
        assert_eq!(
            parse_target(b"b:/base/f"),
            Some(Target::BasePath("/base/f".into()))
        );
        // Malformed: bad ino, unknown tag, missing colon, empty.
        assert_eq!(parse_target(b"s:x"), None);
        assert_eq!(parse_target(b"Z:/f"), None);
        assert_eq!(parse_target(b"s"), None);
        assert_eq!(parse_target(b""), None);
        // Uppercase (the old tags, and the record-tag letters) are not pre tags.
        assert_eq!(parse_target(b"A"), None);
        assert_eq!(parse_target(b"S:7"), None);
        assert_eq!(parse_target(b"B:/f"), None);
    }

    #[test]
    fn parse_stage_and_delete_pre() {
        // S: 4th field is the tagged pre-op target.
        let r = parse(b"S\0/a\01\0b:/a\n").unwrap();
        assert!(matches!(
            &r[0],
            Record::Action(Action::Stage { pre: Target::BasePath(p), .. }) if p == "/a"
        ));
        let r = parse(b"S\0/a\01\0a\n").unwrap();
        assert!(matches!(
            &r[0],
            Record::Action(Action::Stage {
                pre: Target::Absence,
                ..
            })
        ));
        let r = parse(b"S\0/a\02\0s:9\n").unwrap();
        assert!(matches!(
            &r[0],
            Record::Action(Action::Stage {
                pre: Target::StagedFile(9),
                ..
            })
        ));
        // D: 3rd field is the tagged pre-op target.
        let r = parse(b"D\0/a\0b:/a\n").unwrap();
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
        let r = parse(b"R\0/dst\0/src\0b:/base/src\0s:4\n").unwrap();
        assert!(matches!(
            &r[0],
            Record::Action(Action::Rename { dst, src, src_pre: Target::BasePath(sp), dst_pre: Target::StagedFile(4) })
                if dst == "/dst" && src == "/src" && sp == "/base/src"
        ));
    }

    #[test]
    fn parse_snapshot_record() {
        let records = parse(b"S\0/a\01\0a\nP\0build\nS\0/a\02\0a\n").unwrap();
        assert_eq!(records.len(), 3);
        assert!(
            matches!(&records[1], Record::Marker(Marker::Snapshot { name }) if name == "build")
        );
    }

    #[test]
    fn parse_travel_record() {
        let records = parse(b"T\x002\n").unwrap();
        assert_eq!(records.len(), 1);
        match &records[0] {
            Record::Marker(Marker::Travel { target_gen }) => {
                assert_eq!(*target_gen, 2);
            }
            _ => panic!("expected Travel record"),
        }
    }

    #[test]
    fn malformed_p_record_too_few_fields_skipped() {
        // P with only the tag (needs a name) — should be skipped.
        let records = parse(b"P\nS\0/good\01\0a\n").unwrap();
        assert_eq!(records.len(), 1, "bare P skipped: {records:?}");
        assert!(matches!(
            &records[0],
            Record::Action(Action::Stage { path, ino: 1, .. }) if path == "/good"
        ));
    }

    #[test]
    fn malformed_t_record_skipped() {
        // T with no target, and T with a non-numeric target — both skipped.
        let records = parse(b"T\nT\0nope\nS\0/good\01\0a\n").unwrap();
        assert_eq!(records.len(), 1, "bare/non-numeric T skipped: {records:?}");
        assert!(matches!(
            &records[0],
            Record::Action(Action::Stage { path, ino: 1, .. }) if path == "/good"
        ));
    }

    // ── Path tests ────────────────────────────────────────────────────

    #[test]
    fn parse_entry_full_path() {
        let records = parse(b"S\0/src/main.rs\01\0a\n").unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            matches!(&records[0], Record::Action(Action::Stage { path, ino: 1, .. }) if path == "/src/main.rs")
        );
    }

    // ── Malformed record tests ─────────────────────────────────────────

    #[test]
    fn malformed_s_record_too_few_fields_skipped() {
        // S record missing fields (needs path, ino, preimage) — should be skipped
        let records = parse(b"S\0/file\nS\0/good\02\0a\n").unwrap();
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
        let records = parse(b"D\nS\0/good\01\0a\n").unwrap();
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
        let records = parse(b"R\0/dst\0/src\0a\nS\0/good\01\0a\n").unwrap();
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
        let records = parse(b"D\0/foo\0a\n").unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            matches!(&records[0], Record::Action(Action::Delete { path, .. }) if path == "/foo")
        );
    }

    // ── Note (G/C) record tests ────────────────────────────────────────

    #[test]
    fn parse_gate_records() {
        let records = parse(b"G\0/etc/hosts\0r\0d\nG\0/a\0w\0y\nG\0/b\0r\0n\n").unwrap();
        assert_eq!(records.len(), 3);
        assert!(matches!(
            &records[0],
            Record::Note(Note::Gate { path, op: Op::Read, result: GateResult::DirectDeny }) if path == "/etc/hosts"
        ));
        assert!(matches!(
            &records[1],
            Record::Note(Note::Gate {
                result: GateResult::AskAllow,
                ..
            })
        ));
        assert!(matches!(
            &records[2],
            Record::Note(Note::Gate {
                result: GateResult::AskDeny,
                ..
            })
        ));
    }

    #[test]
    fn parse_configure_records() {
        let records = parse(b"C\0/etc\0r\nC\0/etc\0u\n").unwrap();
        assert_eq!(records.len(), 2);
        assert!(matches!(
            &records[0],
            Record::Note(Note::Configure { path, policy: Policy::ReadOnly }) if path == "/etc"
        ));
        assert!(matches!(
            &records[1],
            Record::Note(Note::Configure {
                policy: Policy::Unset,
                ..
            })
        ));
    }

    #[test]
    fn old_a_b_records_are_ignored() {
        let records = parse(b"A\0/etc/hosts\0r\0n\nB\0/etc/passwd\0w\0/etc\n").unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn malformed_gate_and_configure_records_are_skipped() {
        let records = parse(
            b"G\0/a\0r\nG\0/a\0x\0d\nG\0/a\0read\0d\nG\0/a\0r\0deny\nG\0/a\0r\0x\nG\0/a\0r\0d\0extra\nC\0/a\nC\0/a\0x\nC\0/a\0deny\nC\0/a\0d\0extra\n",
        )
        .unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn gate_interleaved_with_actions() {
        let records = parse(b"S\0/a\01\0a\nG\0/etc/passwd\0w\0d\nD\0/a\0a\n").unwrap();
        assert_eq!(records.len(), 3);
        assert!(matches!(
            &records[0],
            Record::Action(Action::Stage { path, ino: 1, .. }) if path == "/a"
        ));
        assert!(matches!(
            &records[1],
            Record::Note(Note::Gate { path, op: Op::Write, result: GateResult::DirectDeny })
                if path == "/etc/passwd"
        ));
        assert!(matches!(
            &records[2],
            Record::Action(Action::Delete { path, .. }) if path == "/a"
        ));
    }
}
