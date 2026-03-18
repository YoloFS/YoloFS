// agfs CLI — journal.rs
//
// Parse the append-only journal and define its record types.
//
// Record format (NUL-separated fields, newline-terminated):
//   A\0<dir>\0<name>\0<dtype>\0<ino>\n       — add (new file)
//   M\0<dir>\0<name>\0<dtype>\0<ino>\n       — modify (existing file)
//   D\0<dir>\0<name>\n                        — delete
//   R\0<dir>\0<name>\0<dtype>\0<base>\n       — redirect (rename)
//   K\0<id>\0<name>\n                         — checkpoint marker

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub const INO_REDIRECT: u64 = u64::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DType {
    File,
    Dir,
    Link,
}

impl DType {
    pub fn from_char(c: u8) -> Option<DType> {
        match c {
            b'f' => Some(DType::File),
            b'd' => Some(DType::Dir),
            b'l' => Some(DType::Link),
            _ => None,
        }
    }

    pub fn to_libc(&self) -> u8 {
        match self {
            DType::File => libc::DT_REG,
            DType::Dir => libc::DT_DIR,
            DType::Link => libc::DT_LNK,
        }
    }
}

/// A named checkpoint in the journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub id: u64,
    pub name: String,
}

/// A journal record: either an entry (dirent mutation) or a checkpoint.
#[derive(Debug, Clone)]
pub enum Record {
    Added {
        path: String,
        dtype: Option<DType>,
        ino: u64,
    },
    Modified {
        path: String,
        dtype: Option<DType>,
        ino: u64,
    },
    Deleted {
        path: String,
    },
    Redirect {
        path: String,
        dtype: Option<DType>,
        base: String,
    },
    Checkpoint(Checkpoint),
}

/// A parsed journal: records and the byte offset after each record.
pub struct Journal {
    pub records: Vec<Record>,
    /// `offsets[i]` is the byte offset in the file just past record `i`.
    offsets: Vec<u64>,
}

impl Journal {
    /// Byte offset just past the given record index.
    pub fn offset(&self, record_idx: usize) -> Option<u64> {
        self.offsets.get(record_idx).copied()
    }
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
            offsets: Vec::new(),
        });
    }
    let data = fs::read(&journal_path).context("reading journal file")?;
    let mut records = Vec::new();
    let mut offsets = Vec::new();
    let mut pos: u64 = 0;

    for line in data.split(|&b| b == b'\n') {
        let line_end = pos + line.len() as u64 + 1; // +1 for the \n
        if line.is_empty() {
            pos = line_end;
            continue;
        }
        let fields: Vec<&[u8]> = line.split(|&b| b == 0).collect();
        if fields.is_empty() {
            pos = line_end;
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
                    offsets.push(line_end);
                }
            }
            b"D" if fields.len() >= 3 => {
                let path = make_path(fields[1], fields[2]);
                records.push(Record::Deleted { path });
                offsets.push(line_end);
            }
            b"R" if fields.len() >= 5 => {
                let path = make_path(fields[1], fields[2]);
                let dtype = parse_dtype(fields[3]);
                let base = String::from_utf8_lossy(fields[4]).to_string();

                records.push(Record::Redirect { path, dtype, base });
                offsets.push(line_end);
            }
            b"K" if fields.len() >= 3 => {
                let id_str = String::from_utf8_lossy(fields[1]);
                let name = String::from_utf8_lossy(fields[2]).to_string();
                if let Ok(id) = id_str.parse::<u64>() {
                    records.push(Record::Checkpoint(Checkpoint { id, name }));
                    offsets.push(line_end);
                }
            }
            _ => {}
        }
        pos = line_end;
    }
    Ok(Journal { records, offsets })
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
    use std::fs;

    fn setup_test_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("inodes")).unwrap();
        dir
    }

    #[test]
    fn read_empty() {
        let dir = setup_test_dir();
        let journal = read(dir.path()).unwrap();
        assert!(journal.records.is_empty());
    }

    #[test]
    fn read_multiple() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // A: added file "a" at root with ino=1, dtype=f
        data.extend_from_slice(b"A\0\0a\0f\01\n");
        // D: deleted "b" at root
        data.extend_from_slice(b"D\0\0b\n");
        // R: redirect "d" at root from /c
        data.extend_from_slice(b"R\0\0d\0f\0/c\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let journal = read(dir.path()).unwrap();
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
    fn read_modified_record() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"M\0/src\0main.rs\0f\03\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let journal = read(dir.path()).unwrap();
        assert_eq!(journal.records.len(), 1);
        assert!(
            matches!(&journal.records[0], Record::Modified { path, ino: 3, dtype: Some(DType::File) } if path == "/src/main.rs"),
            "M record should parse as Modified, got: {:?}",
            journal.records[0]
        );
    }

    #[test]
    fn inode_path_format() {
        let dir = setup_test_dir();
        let p = inode_path(dir.path(), 42);
        assert!(p.ends_with("inodes/42"));
    }

    // ── Checkpoint tests ───────────────────────────────────────────────

    #[test]
    fn read_checkpoint_record() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0\0a\0f\01\n");
        data.extend_from_slice(b"K\01\0build\n");
        data.extend_from_slice(b"A\0\0a\0f\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let journal = read(dir.path()).unwrap();
        assert_eq!(journal.records.len(), 3);
        assert!(
            matches!(&journal.records[1], Record::Checkpoint(c) if c.id == 1 && c.name == "build")
        );
    }

    #[test]
    fn truncate_drops_trailing_records() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0\0a\0f\01\n");
        data.extend_from_slice(b"K\01\0chk\n");
        data.extend_from_slice(b"A\0\0b\0f\02\n");
        data.extend_from_slice(b"D\0\0c\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let journal = read(dir.path()).unwrap();
        assert_eq!(journal.records.len(), 4);

        // Truncate after the checkpoint (record index 1)
        truncate(dir.path(), journal.offset(1).unwrap()).unwrap();

        let after = read(dir.path()).unwrap();
        assert_eq!(after.records.len(), 2);
        assert!(matches!(&after.records[0], Record::Added { path, .. } if path == "/a"));
        assert!(matches!(&after.records[1], Record::Checkpoint(c) if c.id == 1 && c.name == "chk"));
    }

    #[test]
    fn truncate_at_last_record_is_noop() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0\0a\0f\01\n");
        data.extend_from_slice(b"K\01\0chk\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let journal = read(dir.path()).unwrap();
        assert_eq!(journal.records.len(), 2);

        let size_before = fs::metadata(dir.path().join("journal")).unwrap().len();
        truncate(dir.path(), journal.offset(1).unwrap()).unwrap();
        let size_after = fs::metadata(dir.path().join("journal")).unwrap().len();

        assert_eq!(
            size_before, size_after,
            "truncating at last record should not change file size"
        );
        let after = read(dir.path()).unwrap();
        assert_eq!(after.records.len(), 2);
    }

    #[test]
    fn offsets_are_byte_accurate() {
        let dir = setup_test_dir();
        let r0 = b"A\0\0a\0f\01\n";
        let r1 = b"K\01\0chk\n";
        let r2 = b"D\0\0longpath\n";
        let mut data = Vec::new();
        data.extend_from_slice(r0);
        data.extend_from_slice(r1);
        data.extend_from_slice(r2);
        fs::write(dir.path().join("journal"), &data).unwrap();

        let journal = read(dir.path()).unwrap();
        assert_eq!(journal.records.len(), 3);
        assert_eq!(journal.offsets[0], r0.len() as u64);
        assert_eq!(journal.offsets[1], (r0.len() + r1.len()) as u64);
        assert_eq!(journal.offsets[2], (r0.len() + r1.len() + r2.len()) as u64);
    }

    #[test]
    fn read_entry_in_subdirectory() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // File "main.rs" in directory "/src"
        data.extend_from_slice(b"A\0/src\0main.rs\0f\01\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let journal = read(dir.path()).unwrap();
        assert_eq!(journal.records.len(), 1);
        assert!(
            matches!(&journal.records[0], Record::Added { path, ino: 1, dtype: Some(DType::File) } if path == "/src/main.rs")
        );
    }

    #[test]
    fn read_directory_and_symlink_dtypes() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0\0mydir\0d\01\n");
        data.extend_from_slice(b"A\0\0mylink\0l\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let journal = read(dir.path()).unwrap();
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
    fn read_entry_missing_dtype_is_none() {
        let dir = setup_test_dir();
        // A record with empty dtype field
        fs::write(dir.path().join("journal"), b"A\0\0file\0\01\n").unwrap();

        let journal = read(dir.path()).unwrap();
        assert_eq!(journal.records.len(), 1);
        assert!(
            matches!(&journal.records[0], Record::Added { dtype: None, .. }),
            "empty dtype field should parse as None, got: {:?}",
            journal.records[0]
        );
    }

    #[test]
    fn read_entry_invalid_dtype_is_none() {
        let dir = setup_test_dir();
        // A record with invalid dtype char 'x'
        fs::write(dir.path().join("journal"), b"A\0\0file\0x\01\n").unwrap();

        let journal = read(dir.path()).unwrap();
        assert_eq!(journal.records.len(), 1);
        assert!(
            matches!(&journal.records[0], Record::Added { dtype: None, .. }),
            "invalid dtype char should parse as None, got: {:?}",
            journal.records[0]
        );
    }

    #[test]
    fn malformed_a_record_too_few_fields_skipped() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // A record with only 3 fields (needs 5) — should be skipped
        data.extend_from_slice(b"A\0\0file\01\n");
        // Valid record after it
        data.extend_from_slice(b"A\0\0good\0f\02\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let journal = read(dir.path()).unwrap();
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
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // D record with only 1 field (needs 3) — should be skipped
        data.extend_from_slice(b"D\0\n");
        // Valid record after it
        data.extend_from_slice(b"A\0\0good\0f\01\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let journal = read(dir.path()).unwrap();
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
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // R record with only 4 fields (needs 5) — should be skipped
        data.extend_from_slice(b"R\0\0file\0f\n");
        // Valid record after it
        data.extend_from_slice(b"A\0\0good\0f\01\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let journal = read(dir.path()).unwrap();
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
