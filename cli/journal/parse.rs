// agfs CLI — journal/parse.rs
//
// Parse the append-only journal file.
//
// Record format (NUL-separated fields, newline-terminated):
//   A\0<dir>\0<name>\0<dtype>\0<ino>\n       — add (staged, new path)
//   M\0<dir>\0<name>\0<dtype>\0<ino>\n       — modify (staged, existing path)
//   D\0<dir>\0<name>\n                        — delete
//   R\0<dir>\0<name>\0<dtype>\0<base>\n       — redirect (rename, new path)
//   P\0<dir>\0<name>\0<dtype>\0<base>\n       — replace (rename, existing path)
//   K\0<gen>\0<name>\n                         — checkpoint marker
//   S\0<gen>\0<target_gen>\n                   — restore marker

use super::types::*;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// A parsed journal.
pub struct Journal {
    pub records: Vec<Record>,
}

fn make_path(dir_field: &[u8], name_field: &[u8]) -> String {
    let dir = String::from_utf8_lossy(dir_field);
    let name = String::from_utf8_lossy(name_field);
    if dir.is_empty() {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn parse_dtype(field: &[u8]) -> Option<DType> {
    if field.len() == 1 {
        DType::from_char(field[0])
    } else {
        None
    }
}

/// Read and parse the journal file.
pub fn read(agfs_dir: &Path) -> Result<Journal> {
    let journal_path = agfs_dir.join("journal");
    if !journal_path.exists() {
        return Ok(Journal {
            records: Vec::new(),
        });
    }
    let data = fs::read(&journal_path).context("reading journal file")?;
    parse(&data)
}

/// Parse journal bytes into records.
pub fn parse(data: &[u8]) -> Result<Journal> {
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
            b"A" | b"M" if fields.len() >= 5 => {
                let path = make_path(fields[1], fields[2]);
                let dtype = parse_dtype(fields[3]);
                let ino_str = String::from_utf8_lossy(fields[4]);

                if let Ok(ino) = ino_str.parse::<u64>() {
                    if tag == b"A" {
                        records.push(Record::Added { path, dtype, ino });
                    } else {
                        records.push(Record::Modified { path, dtype, ino });
                    }
                }
            }
            b"D" if fields.len() >= 3 => {
                let path = make_path(fields[1], fields[2]);
                records.push(Record::Deleted { path });
            }
            b"R" | b"P" if fields.len() >= 5 => {
                let path = make_path(fields[1], fields[2]);
                let dtype = parse_dtype(fields[3]);
                let base = String::from_utf8_lossy(fields[4]).to_string();

                if tag == b"R" {
                    records.push(Record::Redirect { path, dtype, base });
                } else {
                    records.push(Record::Replace { path, dtype, base });
                }
            }
            b"K" if fields.len() >= 3 => {
                let gen_str = String::from_utf8_lossy(fields[1]);
                let name = String::from_utf8_lossy(fields[2]).to_string();
                if let Ok(gen_id) = gen_str.parse::<u64>() {
                    records.push(Record::Checkpoint(Checkpoint { gen_id, name }));
                }
            }
            b"S" if fields.len() >= 3 => {
                let gen_str = String::from_utf8_lossy(fields[1]);
                let target_str = String::from_utf8_lossy(fields[2]);
                if let (Ok(gen_id), Ok(target_gen)) =
                    (gen_str.parse::<u64>(), target_str.parse::<u64>())
                {
                    records.push(Record::Restore { gen_id, target_gen });
                }
            }
            _ => {}
        }
    }
    Ok(Journal { records })
}

/// Get the staged inode path for a given ino.
pub fn inode_path(agfs_dir: &Path, ino: u64) -> PathBuf {
    agfs_dir.join("inodes").join(ino.to_string())
}

/// Truncate the journal to the given byte offset.
/// Preserves the inode so the kernel's O_APPEND fd stays valid.
pub fn truncate(agfs_dir: &Path, offset: u64) -> Result<()> {
    let journal_path = agfs_dir.join("journal");
    let f = fs::OpenOptions::new()
        .write(true)
        .open(&journal_path)
        .context("opening journal")?;
    f.set_len(offset).context("truncating journal")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // ── IO tests (genuinely need a filesystem) ─────────────────────────

    #[test]
    fn read_missing_journal_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let journal = read(dir.path()).unwrap();
        assert!(journal.records.is_empty());
    }

    #[test]
    fn inode_path_format() {
        let p = inode_path(Path::new("/tmp/fake"), 42);
        assert!(p.ends_with("inodes/42"));
    }

    // ── Parse tests (pure in-memory) ───────────────────────────────────

    #[test]
    fn parse_multiple() {
        let journal = parse(b"A\0\0a\0f\01\nD\0\0b\nR\0\0d\0f\0/c\n").unwrap();
        assert_eq!(journal.records.len(), 3);
        assert!(
            matches!(&journal.records[0], Record::Added { path, ino: 1, dtype: Some(DType::File) } if path == "/a")
        );
        assert!(matches!(&journal.records[1], Record::Deleted { path } if path == "/b"));
        assert!(
            matches!(&journal.records[2], Record::Redirect { path, base, dtype: Some(DType::File) } if path == "/d" && base == "/c")
        );
    }

    #[test]
    fn parse_modified_record() {
        let journal = parse(b"M\0/src\0main.rs\0f\03\n").unwrap();
        assert_eq!(journal.records.len(), 1);
        assert!(
            matches!(&journal.records[0], Record::Modified { path, ino: 3, dtype: Some(DType::File) } if path == "/src/main.rs"),
            "M record should parse as Modified, got: {:?}",
            journal.records[0]
        );
    }

    // ── Checkpoint tests ───────────────────────────────────────────────

    #[test]
    fn parse_checkpoint_record() {
        let journal = parse(b"A\0\0a\0f\01\nK\01\0build\nA\0\0a\0f\02\n").unwrap();
        assert_eq!(journal.records.len(), 3);
        assert!(
            matches!(&journal.records[1], Record::Checkpoint(c) if c.gen_id == 1 && c.name == "build")
        );
    }

    #[test]
    fn parse_restore_record() {
        let journal = parse(b"S\x004\x002\n").unwrap();
        assert_eq!(journal.records.len(), 1);
        match &journal.records[0] {
            Record::Restore { gen_id, target_gen } => {
                assert_eq!(*gen_id, 4);
                assert_eq!(*target_gen, 2);
            }
            _ => panic!("expected Restore record"),
        }
    }

    // ── Path construction tests ────────────────────────────────────────

    #[test]
    fn parse_entry_in_subdirectory() {
        let journal = parse(b"A\0/src\0main.rs\0f\01\n").unwrap();
        assert_eq!(journal.records.len(), 1);
        assert!(
            matches!(&journal.records[0], Record::Added { path, ino: 1, dtype: Some(DType::File) } if path == "/src/main.rs")
        );
    }

    // ── DType tests ────────────────────────────────────────────────────

    #[test]
    fn parse_directory_and_symlink_dtypes() {
        let journal = parse(b"A\0\0mydir\0d\01\nA\0\0mylink\0l\02\n").unwrap();
        assert_eq!(journal.records.len(), 2);
        assert!(matches!(
            &journal.records[0],
            Record::Added {
                dtype: Some(DType::Dir),
                ..
            }
        ));
        assert!(matches!(
            &journal.records[1],
            Record::Added {
                dtype: Some(DType::Link),
                ..
            }
        ));
    }

    #[test]
    fn dtype_to_libc() {
        assert_eq!(DType::File.to_libc(), libc::DT_REG);
        assert_eq!(DType::Dir.to_libc(), libc::DT_DIR);
        assert_eq!(DType::Link.to_libc(), libc::DT_LNK);
    }

    #[test]
    fn parse_entry_missing_dtype_is_none() {
        // A record with empty dtype field
        let journal = parse(b"A\0\0file\0\01\n").unwrap();
        assert_eq!(journal.records.len(), 1);
        assert!(
            matches!(&journal.records[0], Record::Added { dtype: None, .. }),
            "empty dtype field should parse as None, got: {:?}",
            journal.records[0]
        );
    }

    #[test]
    fn parse_entry_invalid_dtype_is_none() {
        // A record with invalid dtype char 'x'
        let journal = parse(b"A\0\0file\0x\01\n").unwrap();
        assert_eq!(journal.records.len(), 1);
        assert!(
            matches!(&journal.records[0], Record::Added { dtype: None, .. }),
            "invalid dtype char should parse as None, got: {:?}",
            journal.records[0]
        );
    }

    // ── Malformed record tests ─────────────────────────────────────────

    #[test]
    fn malformed_a_record_too_few_fields_skipped() {
        // A record with only 3 fields (needs 5) — should be skipped
        let journal = parse(b"A\0\0file\01\nA\0\0good\0f\02\n").unwrap();
        assert_eq!(
            journal.records.len(),
            1,
            "malformed record should be skipped: {:?}",
            journal.records
        );
        assert!(matches!(
            &journal.records[0],
            Record::Added { path, ino: 2, .. } if path == "/good"
        ));
    }

    #[test]
    fn malformed_d_record_too_few_fields_skipped() {
        // D record with only 1 field (needs 3) — should be skipped
        let journal = parse(b"D\0\nA\0\0good\0f\01\n").unwrap();
        assert_eq!(
            journal.records.len(),
            1,
            "malformed D record should be skipped: {:?}",
            journal.records
        );
        assert!(matches!(
            &journal.records[0],
            Record::Added { path, ino: 1, .. } if path == "/good"
        ));
    }

    #[test]
    fn malformed_r_record_too_few_fields_skipped() {
        // R record with only 4 fields (needs 5) — should be skipped
        let journal = parse(b"R\0\0file\0f\nA\0\0good\0f\01\n").unwrap();
        assert_eq!(
            journal.records.len(),
            1,
            "malformed R record should be skipped: {:?}",
            journal.records
        );
        assert!(matches!(
            &journal.records[0],
            Record::Added { path, ino: 1, .. } if path == "/good"
        ));
    }
}
