// agfs CLI — journal.rs
//
// Parse the append-only journal and define its record types.
//
// Record format (NUL-separated fields, newline-terminated):
//   A\0<path>\0<ino>\n   — content/dir in inodes/<ino>
//   D\0<path>\n          — deleted
//   R\0<old>\0<new>\n    — rename
//   K\0<id>\0<name>\n    — checkpoint marker

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// A raw journal record.
#[derive(Debug, Clone)]
pub enum Record {
    Add { path: String, ino: u64 },
    Delete { path: String },
    Rename { old_path: String, new_path: String },
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
            b"A" if fields.len() >= 3 => {
                let path = String::from_utf8_lossy(fields[1]).to_string();
                let ino_str = String::from_utf8_lossy(fields[2]);
                if let Ok(ino) = ino_str.parse::<u64>() {
                    records.push(Record::Add { path, ino });
                    offsets.push(line_end);
                }
            }
            b"D" if fields.len() >= 2 => {
                let path = String::from_utf8_lossy(fields[1]).to_string();
                records.push(Record::Delete { path });
                offsets.push(line_end);
            }
            b"R" if fields.len() >= 3 => {
                let old_path = String::from_utf8_lossy(fields[1]).to_string();
                let new_path = String::from_utf8_lossy(fields[2]).to_string();
                records.push(Record::Rename { old_path, new_path });
                offsets.push(line_end);
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
        data.extend_from_slice(b"A\0/a\01\n");
        data.extend_from_slice(b"D\0/b\n");
        data.extend_from_slice(b"R\0/c\0/d\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let journal = read(dir.path()).unwrap();
        assert_eq!(journal.records.len(), 3);
        assert!(
            matches!(&journal.records[0], Record::Add { path, ino } if path == "/a" && *ino == 1)
        );
        assert!(matches!(&journal.records[1], Record::Delete { path } if path == "/b"));
        assert!(
            matches!(&journal.records[2], Record::Rename { old_path, new_path }
            if old_path == "/c" && new_path == "/d")
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
        data.extend_from_slice(b"A\0/a\01\n");
        data.extend_from_slice(b"K\01\0build\n");
        data.extend_from_slice(b"A\0/a\02\n");
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
        data.extend_from_slice(b"A\0/a\01\n");
        data.extend_from_slice(b"K\01\0snap\n");
        data.extend_from_slice(b"A\0/b\02\n");
        data.extend_from_slice(b"D\0/c\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let journal = read(dir.path()).unwrap();
        assert_eq!(journal.records.len(), 4);

        // Truncate after the checkpoint (record index 1)
        truncate(&journal, dir.path(), 1).unwrap();

        let after = read(dir.path()).unwrap();
        assert_eq!(after.records.len(), 2);
        assert!(matches!(&after.records[0], Record::Add { path, .. } if path == "/a"));
        assert!(matches!(&after.records[1], Record::Checkpoint { id: 1, name } if name == "snap"));
    }

    #[test]
    fn truncate_at_last_record_is_noop() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/a\01\n");
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
        let r0 = b"A\0/a\01\n";
        let r1 = b"K\01\0snap\n";
        let r2 = b"D\0/longpath\n";
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
}
