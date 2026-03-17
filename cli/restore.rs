// agfs CLI — restore.rs
//
// `agfs restore <name|id>` — restore to a previous checkpoint.

use crate::utils::to_base_path;
use crate::{ioctl, journal, resolve};
use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;
use std::path::Path;

/// Determine d_type from a filesystem path.
fn d_type_from_path(path: &Path) -> Result<u8> {
    let m = fs::symlink_metadata(path).with_context(|| format!("stat {}", path.display()))?;
    Ok(if m.file_type().is_symlink() {
        libc::DT_LNK
    } else if m.is_dir() {
        libc::DT_DIR
    } else {
        libc::DT_REG
    })
}

/// Convert a resolved Change list into restore entries for the ioctl.
fn changes_to_entries(
    agfs_dir: &Path,
    changes: &[resolve::Change],
) -> Result<Vec<ioctl::AgfsIocRestoreEntry>> {
    let mut entries = Vec::new();

    for change in changes {
        match change {
            resolve::Change::Added { path, ino } | resolve::Change::Modified { path, ino } => {
                let staged = journal::inode_path(agfs_dir, *ino);
                entries.push(ioctl::AgfsIocRestoreEntry::new(
                    path,
                    *ino,
                    "",
                    d_type_from_path(&staged)?,
                ));
            }
            resolve::Change::Deleted(path) => {
                entries.push(ioctl::AgfsIocRestoreEntry::new(path, 0, "", 0));
            }
            resolve::Change::Renamed { from, to } => {
                entries.push(ioctl::AgfsIocRestoreEntry::new(from, 0, "", 0));
                entries.push(ioctl::AgfsIocRestoreEntry::new(
                    to,
                    0,
                    from,
                    d_type_from_path(&to_base_path(from))?,
                ));
            }
            resolve::Change::RenamedModified { from, to, ino } => {
                let staged = journal::inode_path(agfs_dir, *ino);
                entries.push(ioctl::AgfsIocRestoreEntry::new(from, 0, "", 0));
                entries.push(ioctl::AgfsIocRestoreEntry::new(
                    to,
                    *ino,
                    "",
                    d_type_from_path(&staged)?,
                ));
            }
        }
    }

    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

pub fn run(checkpoint_name: &str) -> Result<()> {
    let agfs = crate::utils::session_dir()?;

    let journal = journal::read(&agfs)?;
    let chk_idx = resolve::find_checkpoint_index(&journal.records, checkpoint_name)?;

    let (checkpoint_gen, chk_label) = match &journal.records[chk_idx] {
        journal::Record::Checkpoint { id, name } => (*id, name.as_str()),
        _ => unreachable!("find_checkpoint_index returned non-checkpoint record"),
    };

    let changes = resolve::resolve(&journal.records[..=chk_idx])?;
    let entries = changes_to_entries(&agfs, &changes)?;

    // Restore kernel state first — if this fails (e.g. EBUSY), the
    // journal is still intact and the operation can be retried.
    let ctl_file = ioctl::open(&agfs).context("opening ctl for restore")?;
    ioctl::restore(&ctl_file, checkpoint_gen, &entries).context("ioctl RESTORE")?;

    // Truncate journal after the checkpoint record — preserves the inode
    // so the kernel's O_APPEND fd stays valid.
    journal::truncate(&journal, &agfs, chk_idx)?;

    println!(
        "{}",
        format!(
            "Restored to checkpoint \"{chk_label}\" ({} staged change{}).",
            changes.len(),
            crate::utils::plural(changes.len())
        )
        .green()
        .bold()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ioctl::AGFS_PATH_MAX;
    use crate::resolve::Change;

    fn entry_path(e: &ioctl::AgfsIocRestoreEntry) -> &str {
        let end = e.path.iter().position(|&b| b == 0).unwrap_or(AGFS_PATH_MAX);
        std::str::from_utf8(&e.path[..end]).unwrap()
    }

    fn entry_base_path(e: &ioctl::AgfsIocRestoreEntry) -> &str {
        let end = e
            .base_path
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(AGFS_PATH_MAX);
        std::str::from_utf8(&e.base_path[..end]).unwrap()
    }

    #[test]
    fn added_produces_single_entry() {
        let dir = tempfile::tempdir().unwrap();
        let inodes = dir.path().join("inodes");
        fs::create_dir(&inodes).unwrap();
        fs::write(inodes.join("1"), "content").unwrap();

        let changes = vec![Change::Added {
            path: "/src/main.rs".into(),
            ino: 1,
        }];
        let entries = changes_to_entries(dir.path(), &changes).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entry_path(&entries[0]), "/src/main.rs");
        assert_eq!(entries[0].ino, 1);
        assert_eq!(entries[0].d_type, libc::DT_REG);
        assert_eq!(entry_base_path(&entries[0]), "");
    }

    #[test]
    fn deleted_produces_zero_entry() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("inodes")).unwrap();

        let changes = vec![Change::Deleted("/old.txt".into())];
        let entries = changes_to_entries(dir.path(), &changes).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entry_path(&entries[0]), "/old.txt");
        assert_eq!(entries[0].ino, 0);
        assert_eq!(entry_base_path(&entries[0]), "");
    }

    #[test]
    fn renamed_produces_delete_and_redirect() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("inodes")).unwrap();

        // Use a real existing file so d_type_from_path can stat it
        let base = tempfile::NamedTempFile::new().unwrap();
        let from = base.path().to_str().unwrap().to_string();

        let changes = vec![Change::Renamed {
            from: from.clone(),
            to: "/b.txt".into(),
        }];
        let entries = changes_to_entries(dir.path(), &changes).unwrap();
        assert_eq!(entries.len(), 2);

        let del = entries.iter().find(|e| entry_path(e) == from).unwrap();
        assert_eq!(del.ino, 0);

        let redirect = entries.iter().find(|e| entry_path(e) == "/b.txt").unwrap();
        assert_eq!(redirect.ino, 0);
        assert_eq!(entry_base_path(redirect), from);
    }

    #[test]
    fn renamed_modified_produces_delete_and_ino() {
        let dir = tempfile::tempdir().unwrap();
        let inodes = dir.path().join("inodes");
        fs::create_dir(&inodes).unwrap();
        fs::write(inodes.join("5"), "new content").unwrap();

        let changes = vec![Change::RenamedModified {
            from: "/old.rs".into(),
            to: "/new.rs".into(),
            ino: 5,
        }];
        let entries = changes_to_entries(dir.path(), &changes).unwrap();
        assert_eq!(entries.len(), 2);

        // Sorted: /new.rs before /old.rs
        assert_eq!(entry_path(&entries[0]), "/new.rs");
        assert_eq!(entries[0].ino, 5);
        assert_eq!(entry_base_path(&entries[0]), "");

        assert_eq!(entry_path(&entries[1]), "/old.rs");
        assert_eq!(entries[1].ino, 0);
    }

    #[test]
    fn entries_sorted_by_path() {
        let dir = tempfile::tempdir().unwrap();
        let inodes = dir.path().join("inodes");
        fs::create_dir(&inodes).unwrap();
        fs::write(inodes.join("1"), "").unwrap();
        fs::write(inodes.join("2"), "").unwrap();
        fs::create_dir(inodes.join("3")).unwrap();

        let changes = vec![
            Change::Added {
                path: "/z/file.rs".into(),
                ino: 1,
            },
            Change::Added {
                path: "/a/file.rs".into(),
                ino: 2,
            },
            Change::Added {
                path: "/a".into(),
                ino: 3,
            },
        ];
        let entries = changes_to_entries(dir.path(), &changes).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entry_path(&entries[0]), "/a");
        assert_eq!(entry_path(&entries[1]), "/a/file.rs");
        assert_eq!(entry_path(&entries[2]), "/z/file.rs");
    }

    #[test]
    fn directory_inode_gets_dt_dir() {
        let dir = tempfile::tempdir().unwrap();
        let inodes = dir.path().join("inodes");
        fs::create_dir(&inodes).unwrap();
        fs::create_dir(inodes.join("1")).unwrap();

        let changes = vec![Change::Added {
            path: "/newdir".into(),
            ino: 1,
        }];
        let entries = changes_to_entries(dir.path(), &changes).unwrap();
        assert_eq!(entries[0].d_type, libc::DT_DIR);
    }

    #[test]
    fn symlink_inode_gets_dt_lnk() {
        let dir = tempfile::tempdir().unwrap();
        let inodes = dir.path().join("inodes");
        fs::create_dir(&inodes).unwrap();
        std::os::unix::fs::symlink("target", inodes.join("1")).unwrap();

        let changes = vec![Change::Added {
            path: "/link".into(),
            ino: 1,
        }];
        let entries = changes_to_entries(dir.path(), &changes).unwrap();
        assert_eq!(entries[0].d_type, libc::DT_LNK);
    }

    #[test]
    fn new_entry_truncates_long_path() {
        let long = "/".to_string() + &"a".repeat(300);
        let entry = ioctl::AgfsIocRestoreEntry::new(&long, 1, "", libc::DT_REG);
        assert_eq!(entry.path[AGFS_PATH_MAX - 1], 0);
        assert_eq!(entry.path[AGFS_PATH_MAX - 2], b'a');
    }

    #[test]
    fn empty_changes_produces_no_entries() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("inodes")).unwrap();

        let entries = changes_to_entries(dir.path(), &[]).unwrap();
        assert!(entries.is_empty());
    }

    /// Renamed directory must produce DT_DIR, not DT_REG.
    #[test]
    fn renamed_directory_gets_dt_dir() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("inodes")).unwrap();

        // Create the "base" directory that the rename came from.
        // to_base_path("/mydir") resolves to "/mydir" on the host,
        // so we create a real temp dir and use its path as `from`.
        let base_dir = tempfile::tempdir().unwrap();
        let from = base_dir.path().to_str().unwrap().to_string();

        let changes = vec![Change::Renamed {
            from: from.clone(),
            to: "/newdir".into(),
        }];
        let entries = changes_to_entries(dir.path(), &changes).unwrap();

        // Find the "to" entry (the one with base_path set)
        let to_entry = entries.iter().find(|e| entry_base_path(e) == from).unwrap();
        assert_eq!(
            to_entry.d_type,
            libc::DT_DIR,
            "renamed directory should have DT_DIR, got {}",
            to_entry.d_type
        );
    }

    /// Renamed symlink must produce DT_LNK, not DT_REG.
    #[test]
    fn renamed_symlink_gets_dt_lnk() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("inodes")).unwrap();

        // Create a real symlink to use as the rename source
        let base_dir = tempfile::tempdir().unwrap();
        let link_path = base_dir.path().join("mylink");
        std::os::unix::fs::symlink("target", &link_path).unwrap();
        let from = link_path.to_str().unwrap().to_string();

        let changes = vec![Change::Renamed {
            from: from.clone(),
            to: "/newlink".into(),
        }];
        let entries = changes_to_entries(dir.path(), &changes).unwrap();

        let to_entry = entries.iter().find(|e| entry_base_path(e) == from).unwrap();
        assert_eq!(
            to_entry.d_type,
            libc::DT_LNK,
            "renamed symlink should have DT_LNK, got {}",
            to_entry.d_type
        );
    }

    /// Missing staged inode must produce an error, not silently default to DT_REG.
    #[test]
    fn missing_inode_errors() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("inodes")).unwrap();
        // Do NOT create inodes/99 — it's missing

        let changes = vec![Change::Added {
            path: "/ghost.txt".into(),
            ino: 99,
        }];
        let result = changes_to_entries(dir.path(), &changes);
        assert!(result.is_err(), "missing inode should produce an error");
    }
}
