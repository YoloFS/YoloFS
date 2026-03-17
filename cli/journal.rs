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

/// Read and parse the journal file.
pub fn read(agfs_dir: &Path) -> Result<Vec<Record>> {
    let journal_path = agfs_dir.join("journal");
    if !journal_path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read(&journal_path).context("reading journal file")?;
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
            b"A" if fields.len() >= 3 => {
                let path = String::from_utf8_lossy(fields[1]).to_string();
                let ino_str = String::from_utf8_lossy(fields[2]);
                if let Ok(ino) = ino_str.parse::<u64>() {
                    records.push(Record::Add { path, ino });
                }
            }
            b"D" if fields.len() >= 2 => {
                let path = String::from_utf8_lossy(fields[1]).to_string();
                records.push(Record::Delete { path });
            }
            b"R" if fields.len() >= 3 => {
                let old_path = String::from_utf8_lossy(fields[1]).to_string();
                let new_path = String::from_utf8_lossy(fields[2]).to_string();
                records.push(Record::Rename { old_path, new_path });
            }
            b"K" if fields.len() >= 3 => {
                let id_str = String::from_utf8_lossy(fields[1]);
                let name = String::from_utf8_lossy(fields[2]).to_string();
                if let Ok(id) = id_str.parse::<u64>() {
                    records.push(Record::Checkpoint { id, name });
                }
            }
            _ => {}
        }
    }
    Ok(records)
}

/// Get the staged inode path for a given ino.
pub fn inode_path(agfs_dir: &Path, ino: u64) -> PathBuf {
    agfs_dir.join("inodes").join(ino.to_string())
}

/// Write raw records back to a journal file (for partial commit rewriting).
pub fn write_records(journal_path: &Path, records: &[Record]) -> Result<()> {
    let mut data = Vec::new();
    for record in records {
        match record {
            Record::Add { path, ino } => {
                data.push(b'A');
                data.push(0);
                data.extend_from_slice(path.as_bytes());
                data.push(0);
                data.extend_from_slice(ino.to_string().as_bytes());
                data.push(b'\n');
            }
            Record::Delete { path } => {
                data.push(b'D');
                data.push(0);
                data.extend_from_slice(path.as_bytes());
                data.push(b'\n');
            }
            Record::Rename { old_path, new_path } => {
                data.push(b'R');
                data.push(0);
                data.extend_from_slice(old_path.as_bytes());
                data.push(0);
                data.extend_from_slice(new_path.as_bytes());
                data.push(b'\n');
            }
            Record::Checkpoint { id, name } => {
                data.push(b'K');
                data.push(0);
                data.extend_from_slice(id.to_string().as_bytes());
                data.push(0);
                data.extend_from_slice(name.as_bytes());
                data.push(b'\n');
            }
        }
    }
    fs::write(journal_path, &data).context("writing journal")
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
        let records = read(dir.path()).unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn read_multiple() {
        let dir = setup_test_dir();
        let mut data = Vec::new();
        data.extend_from_slice(b"A\0/a\01\n");
        data.extend_from_slice(b"D\0/b\n");
        data.extend_from_slice(b"R\0/c\0/d\n");
        fs::write(dir.path().join("journal"), &data).unwrap();

        let records = read(dir.path()).unwrap();
        assert_eq!(records.len(), 3);
        assert!(matches!(&records[0], Record::Add { path, ino } if path == "/a" && *ino == 1));
        assert!(matches!(&records[1], Record::Delete { path } if path == "/b"));
        assert!(matches!(&records[2], Record::Rename { old_path, new_path }
            if old_path == "/c" && new_path == "/d"));
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

        let records = read(dir.path()).unwrap();
        assert_eq!(records.len(), 3);
        assert!(matches!(&records[1], Record::Checkpoint { id: 1, name } if name == "build"));
    }

    #[test]
    fn write_records_roundtrip() {
        let dir = setup_test_dir();
        let records = vec![
            Record::Add {
                path: "/a".into(),
                ino: 1,
            },
            Record::Checkpoint {
                id: 1,
                name: "snap".into(),
            },
            Record::Delete { path: "/b".into() },
            Record::Rename {
                old_path: "/c".into(),
                new_path: "/d".into(),
            },
        ];
        let path = dir.path().join("journal");
        write_records(&path, &records).unwrap();

        let parsed = read(dir.path()).unwrap();
        assert_eq!(parsed.len(), 4);
        assert!(matches!(&parsed[0], Record::Add { path, ino } if path == "/a" && *ino == 1));
        assert!(matches!(&parsed[1], Record::Checkpoint { id: 1, name } if name == "snap"));
        assert!(matches!(&parsed[2], Record::Delete { path } if path == "/b"));
        assert!(matches!(&parsed[3], Record::Rename { old_path, new_path }
            if old_path == "/c" && new_path == "/d"));
    }
}
