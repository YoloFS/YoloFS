// agfs CLI — journal.rs
//
// Parse the append-only journal and define its record types.
//
// Record format (NUL-separated fields, newline-terminated):
//   E\0<dir>\0<name>\0<ino>\0<dtype>\0<base>\n   — entry (staged/deleted/redirect)
//   K\0<id>\0<name>\n                             — checkpoint marker

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub const INO_DELETED: u64 = 0;
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

#[derive(Debug, Clone)]
pub enum Target {
    Staged(u64),
    Redirect(String),
    Deleted,
}

/// A journal record: either an entry (dirent mutation) or a checkpoint.
#[derive(Debug, Clone)]
pub enum Record {
    Entry {
        path: String,
        target: Target,
        dtype: Option<DType>,
    },
    Checkpoint { id: u64, name: String },
}

/// A parsed journal: records and the byte offset after each record.
pub struct Journal {
    pub records: Vec<Record>,
    /// `offsets[i]` is the byte offset in the file just past record `i`.
    offsets: Vec<u64>,
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
            b"E" if fields.len() >= 6 => {
                let dir = String::from_utf8_lossy(fields[1]);
                let name = String::from_utf8_lossy(fields[2]);
                let ino_str = String::from_utf8_lossy(fields[3]);
                let dtype_field = fields[4];
                let base = String::from_utf8_lossy(fields[5]).to_string();

                let path = if dir.is_empty() {
                    format!("/{name}")
                } else {
                    format!("{dir}/{name}")
                };

                let dtype = if dtype_field.len() == 1 {
                    DType::from_char(dtype_field[0])
                } else {
                    None
                };

                if ino_str == "-1" {
                    records.push(Record::Entry {
                        path,
                        target: Target::Redirect(base),
                        dtype,
                    });
                    offsets.push(line_end);
                } else if let Ok(ino) = ino_str.parse::<u64>() {
                    let target = if ino == INO_DELETED {
                        Target::Deleted
                    } else {
                        Target::Staged(ino)
                    };
                    records.push(Record::Entry {
                        path,
                        target,
                        dtype,
                    });
                    offsets.push(line_end);
                }
            }
            b"K" if fields.len() >= 3 => {
                let id_str = String::from_utf8_lossy(fields[1]);
                let name = String::from_utf8_lossy(fields[2]).to_string();
                if let Ok(id) = id_str.parse::<u64>() {
                    records.push(Record::Checkpoint { id, name });
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

/// Truncate the journal after record `record_idx`.
/// Preserves the inode so the kernel's O_APPEND fd stays valid.
pub fn truncate(journal: &Journal, agfs_dir: &Path, record_idx: usize) -> Result<()> {
    let offset = *journal
        .offsets
        .get(record_idx)
        .context("record index out of bounds")?;
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
        // E: staged file "a" at root with ino=1, dtype=f
        data.extend_from_slice(b"E\0\0a\01\0f\0\n");
        // E: deleted "b" at root
        data.extend_from_slice(b"E\0\0b\00\0\0\n");
        // E: redirect "d" at root from /c
        data.extend_from_slice(b"E\0\0d\0-1\0f\0/c\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let journal = read(dir.path()).unwrap();
        assert_eq!(journal.records.len(), 3);
        assert!(
            matches!(&journal.records[0], Record::Entry { path, target: Target::Staged(1), dtype: Some(DType::File) } if path == "/a")
        );
        assert!(
            matches!(&journal.records[1], Record::Entry { path, target: Target::Deleted, .. } if path == "/b")
        );
        assert!(
            matches!(&journal.records[2], Record::Entry { path, target: Target::Redirect(base), dtype: Some(DType::File) } if path == "/d" && base == "/c")
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
        data.extend_from_slice(b"E\0\0a\01\0f\0\n");
        data.extend_from_slice(b"K\01\0build\n");
        data.extend_from_slice(b"E\0\0a\02\0f\0\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let journal = read(dir.path()).unwrap();
        assert_eq!(journal.records.len(), 3);
        assert!(
            matches!(&journal.records[1], Record::Checkpoint { id: 1, name } if name == "build")
        );
    }

    #[test]
    fn truncate_drops_trailing_records() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"E\0\0a\01\0f\0\n");
        data.extend_from_slice(b"K\01\0snap\n");
        data.extend_from_slice(b"E\0\0b\02\0f\0\n");
        data.extend_from_slice(b"E\0\0c\00\0\0\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let journal = read(dir.path()).unwrap();
        assert_eq!(journal.records.len(), 4);

        // Truncate after the checkpoint (record index 1)
        truncate(&journal, dir.path(), 1).unwrap();

        let after = read(dir.path()).unwrap();
        assert_eq!(after.records.len(), 2);
        assert!(matches!(&after.records[0], Record::Entry { path, target: Target::Staged(_), .. } if path == "/a"));
        assert!(matches!(&after.records[1], Record::Checkpoint { id: 1, name } if name == "snap"));
    }

    #[test]
    fn truncate_at_last_record_is_noop() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"E\0\0a\01\0f\0\n");
        data.extend_from_slice(b"K\01\0snap\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let journal = read(dir.path()).unwrap();
        assert_eq!(journal.records.len(), 2);

        let size_before = fs::metadata(dir.path().join("journal")).unwrap().len();
        truncate(&journal, dir.path(), 1).unwrap();
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
        let r0 = b"E\0\0a\01\0f\0\n";
        let r1 = b"K\01\0snap\n";
        let r2 = b"E\0\0longpath\00\0\0\n";
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
        data.extend_from_slice(b"E\0/src\0main.rs\01\0f\0\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let journal = read(dir.path()).unwrap();
        assert_eq!(journal.records.len(), 1);
        assert!(
            matches!(&journal.records[0], Record::Entry { path, target: Target::Staged(1), dtype: Some(DType::File) } if path == "/src/main.rs")
        );
    }

    #[test]
    fn read_directory_and_symlink_dtypes() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"E\0\0mydir\01\0d\0\n");
        data.extend_from_slice(b"E\0\0mylink\02\0l\0\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let journal = read(dir.path()).unwrap();
        assert_eq!(journal.records.len(), 2);
        assert!(
            matches!(&journal.records[0], Record::Entry { dtype: Some(DType::Dir), .. })
        );
        assert!(
            matches!(&journal.records[1], Record::Entry { dtype: Some(DType::Link), .. })
        );
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
        // E record with empty dtype field
        fs::write(dir.path().join("journal"), b"E\0\0file\01\0\0\n").unwrap();

        let journal = read(dir.path()).unwrap();
        assert_eq!(journal.records.len(), 1);
        assert!(
            matches!(&journal.records[0], Record::Entry { dtype: None, .. }),
            "empty dtype field should parse as None, got: {:?}",
            journal.records[0]
        );
    }

    #[test]
    fn read_entry_invalid_dtype_is_none() {
        let dir = setup_test_dir();
        // E record with invalid dtype char 'x'
        fs::write(dir.path().join("journal"), b"E\0\0file\01\0x\0\n").unwrap();

        let journal = read(dir.path()).unwrap();
        assert_eq!(journal.records.len(), 1);
        assert!(
            matches!(&journal.records[0], Record::Entry { dtype: None, .. }),
            "invalid dtype char should parse as None, got: {:?}",
            journal.records[0]
        );
    }

    #[test]
    fn malformed_e_record_too_few_fields_skipped() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        // E record with only 3 fields (needs 6) — should be skipped
        data.extend_from_slice(b"E\0\0file\01\n");
        // Valid record after it
        data.extend_from_slice(b"E\0\0good\02\0f\0\n");
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
            Record::Entry { path, target: Target::Staged(2), .. } if path == "/good"
        ));
    }
}
