// agfs CLI — journal/parse.rs
//
// Parse the append-only journal file.
//
// Record format (NUL-separated fields, newline-terminated):
//   A\0<path>\0<dtype>\0<ino>\n       — Add (staged content at path)
//   D\0<path>\0<dtype>\n              — Delete
//   R\0<dst>\0<src>\0<dtype>\n         — Rename
//   M\0<gen>\0<name>\n                — Mark
//   J\0<gen>\0<target_gen>\n          — Jump

use super::types::*;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

fn parse_dtype(field: &[u8]) -> Option<u8> {
    let s = std::str::from_utf8(field).ok()?;
    let val = s.parse::<u8>().ok()?;
    dtype_valid(val).then_some(val)
}

fn field_str(field: &[u8]) -> String {
    String::from_utf8_lossy(field).into_owned()
}

/// Read and parse the journal file.
pub(super) fn read(agfs_dir: &Path) -> Result<Vec<Record>> {
    let journal_path = agfs_dir.join("journal");
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
            b"A" if fields.len() >= 4 => {
                let path = field_str(fields[1]);
                let dtype = parse_dtype(fields[2]);
                let ino_str = String::from_utf8_lossy(fields[3]);

                if let Ok(ino) = ino_str.parse::<u32>() {
                    records.push(Record::Action(Action::Add { path, dtype, ino }));
                }
            }
            b"D" if fields.len() >= 3 => {
                let path = field_str(fields[1]);
                let dtype = parse_dtype(fields[2]);
                records.push(Record::Action(Action::Delete { path, dtype }));
            }
            b"R" | b"P" if fields.len() >= 4 => {
                let dst = field_str(fields[1]);
                let src = field_str(fields[2]);
                let dtype = parse_dtype(fields[3]);

                records.push(Record::Action(Action::Rename { src, dst, dtype }));
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
        let records = parse(b"A\0/a\08\01\nD\0/b\08\nR\0/d\0/c\08\n").unwrap();
        assert_eq!(records.len(), 3);
        assert!(
            matches!(&records[0], Record::Action(Action::Add { path, ino: 1, dtype: Some(libc::DT_REG) }) if path == "/a")
        );
        assert!(matches!(&records[1], Record::Action(Action::Delete { path, .. }) if path == "/b"));
        assert!(
            matches!(&records[2], Record::Action(Action::Rename { dst, src, dtype: Some(libc::DT_REG) }) if dst == "/d" && src == "/c")
        );
    }

    #[test]
    fn parse_mark_record() {
        let records = parse(b"A\0/a\08\01\nM\01\0build\nA\0/a\08\02\n").unwrap();
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

    #[test]
    fn legacy_four_field_m_record_is_ignored() {
        // Old M records (modify/add alias) had 4 fields; now M is Mark (3 fields).
        let records = parse(b"M\0/src/main.rs\08\03\n").unwrap();
        assert!(records.is_empty(), "legacy 4-field M should be ignored");
    }

    // ── Path tests ────────────────────────────────────────────────────

    #[test]
    fn parse_entry_full_path() {
        let records = parse(b"A\0/src/main.rs\08\01\n").unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            matches!(&records[0], Record::Action(Action::Add { path, ino: 1, dtype: Some(libc::DT_REG) }) if path == "/src/main.rs")
        );
    }

    // ── d_type tests ──────────────────────────────────────────────────

    #[test]
    fn parse_directory_and_symlink_dtypes() {
        let records = parse(b"A\0/mydir\04\01\nA\0/mylink\010\02\n").unwrap();
        assert_eq!(records.len(), 2);
        assert!(matches!(
            &records[0],
            Record::Action(Action::Add {
                dtype: Some(libc::DT_DIR),
                ..
            })
        ));
        assert!(matches!(
            &records[1],
            Record::Action(Action::Add {
                dtype: Some(libc::DT_LNK),
                ..
            })
        ));
    }

    #[test]
    fn parse_entry_missing_dtype_is_none() {
        let records = parse(b"A\0/file\0\01\n").unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            matches!(&records[0], Record::Action(Action::Add { dtype: None, .. })),
            "empty dtype field should parse as None, got: {:?}",
            records[0]
        );
    }

    #[test]
    fn parse_entry_invalid_dtype_is_none() {
        let records = parse(b"A\0/file\0x\01\n").unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            matches!(&records[0], Record::Action(Action::Add { dtype: None, .. })),
            "invalid dtype char should parse as None, got: {:?}",
            records[0]
        );
    }

    // ── Malformed record tests ─────────────────────────────────────────

    #[test]
    fn malformed_a_record_too_few_fields_skipped() {
        // A record with only 3 fields (needs 4) — should be skipped
        let records = parse(b"A\0/file\01\nA\0/good\08\02\n").unwrap();
        assert_eq!(
            records.len(),
            1,
            "malformed record should be skipped: {:?}",
            records
        );
        assert!(matches!(
            &records[0],
            Record::Action(Action::Add { path, ino: 2, .. }) if path == "/good"
        ));
    }

    #[test]
    fn malformed_d_record_too_few_fields_skipped() {
        // D record with only 1 field (needs 3) — should be skipped
        let records = parse(b"D\0\nA\0/good\08\01\n").unwrap();
        assert_eq!(
            records.len(),
            1,
            "malformed D record should be skipped: {:?}",
            records
        );
        assert!(matches!(
            &records[0],
            Record::Action(Action::Add { path, ino: 1, .. }) if path == "/good"
        ));
    }

    #[test]
    fn malformed_r_record_too_few_fields_skipped() {
        // R record with only 3 fields (needs 4) — should be skipped
        let records = parse(b"R\0/file\08\nA\0/good\08\01\n").unwrap();
        assert_eq!(
            records.len(),
            1,
            "malformed R record should be skipped: {:?}",
            records
        );
        assert!(matches!(
            &records[0],
            Record::Action(Action::Add { path, ino: 1, .. }) if path == "/good"
        ));
    }

    #[test]
    fn parse_replace_record() {
        // P records are parsed as Rename (P/R merged)
        let records = parse(b"P\0/dir/newfile\0/dir/oldfile\08\n").unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            matches!(
                &records[0],
                Record::Action(Action::Rename { dst, src, dtype: Some(libc::DT_REG) })
                    if dst == "/dir/newfile" && src == "/dir/oldfile"
            ),
            "P record should parse as Rename, got: {:?}",
            records[0]
        );
    }

    #[test]
    fn parse_deleted_with_dtype() {
        let records = parse(b"D\0/foo\08\n").unwrap();
        assert_eq!(records.len(), 1);
        assert!(
            matches!(&records[0], Record::Action(Action::Delete { path, dtype: Some(libc::DT_REG) }) if path == "/foo")
        );
    }
}
