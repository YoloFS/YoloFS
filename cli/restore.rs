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

/// Intermediate representation of a restore entry with owned path data.
struct RestoreItem {
    path: String,
    ino: u64,
    base_path: String,
    d_type: u8,
}

/// Convert a resolved Change list into restore items (owned data, sortable).
fn changes_to_items(agfs_dir: &Path, changes: &[resolve::Change]) -> Result<Vec<RestoreItem>> {
    let mut items = Vec::new();

    for change in changes {
        match change {
            resolve::Change::Added { path, ino } | resolve::Change::Modified { path, ino } => {
                let staged = journal::inode_path(agfs_dir, *ino);
                items.push(RestoreItem {
                    path: path.clone(),
                    ino: *ino,
                    base_path: String::new(),
                    d_type: d_type_from_path(&staged)?,
                });
            }
            resolve::Change::Deleted(path) => {
                items.push(RestoreItem {
                    path: path.clone(),
                    ino: 0,
                    base_path: String::new(),
                    d_type: 0,
                });
            }
            resolve::Change::Renamed { from, to } => {
                items.push(RestoreItem {
                    path: from.clone(),
                    ino: 0,
                    base_path: String::new(),
                    d_type: 0,
                });
                items.push(RestoreItem {
                    path: to.clone(),
                    ino: 0,
                    base_path: from.clone(),
                    d_type: d_type_from_path(&to_base_path(from))?,
                });
            }
            resolve::Change::RenamedModified { from, to, ino } => {
                let staged = journal::inode_path(agfs_dir, *ino);
                items.push(RestoreItem {
                    path: from.clone(),
                    ino: 0,
                    base_path: String::new(),
                    d_type: 0,
                });
                items.push(RestoreItem {
                    path: to.clone(),
                    ino: *ino,
                    base_path: String::new(),
                    d_type: d_type_from_path(&staged)?,
                });
            }
        }
    }

    items.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(items)
}

/// Convert owned RestoreItems into ioctl entries with pointers into items.
/// The returned entries are valid as long as `items` is alive.
fn items_to_entries(items: &[RestoreItem]) -> Result<Vec<ioctl::AgfsIocRestoreEntry>> {
    items
        .iter()
        .map(|item| {
            let path_len: u16 = item
                .path
                .len()
                .try_into()
                .context("restore path too long")?;
            let base_path_len: u16 = item
                .base_path
                .len()
                .try_into()
                .context("restore base_path too long")?;
            Ok(ioctl::AgfsIocRestoreEntry {
                path_ptr: item.path.as_ptr() as u64,
                path_len,
                d_type: item.d_type,
                _pad1: [0u8; 5],
                ino: item.ino,
                base_path_ptr: item.base_path.as_ptr() as u64,
                base_path_len,
                _pad2: [0u8; 6],
            })
        })
        .collect()
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
    let items = changes_to_items(&agfs, &changes)?;
    let entries = items_to_entries(&items)?;

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
    use crate::resolve::Change;

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
        let items = changes_to_items(dir.path(), &changes).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, "/src/main.rs");
        assert_eq!(items[0].ino, 1);
        assert_eq!(items[0].d_type, libc::DT_REG);
        assert_eq!(items[0].base_path, "");
    }

    #[test]
    fn deleted_produces_zero_entry() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("inodes")).unwrap();

        let changes = vec![Change::Deleted("/old.txt".into())];
        let items = changes_to_items(dir.path(), &changes).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, "/old.txt");
        assert_eq!(items[0].ino, 0);
        assert_eq!(items[0].base_path, "");
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
        let items = changes_to_items(dir.path(), &changes).unwrap();
        assert_eq!(items.len(), 2);

        let del = items.iter().find(|e| e.path == from).unwrap();
        assert_eq!(del.ino, 0);

        let redirect = items.iter().find(|e| e.path == "/b.txt").unwrap();
        assert_eq!(redirect.ino, 0);
        assert_eq!(redirect.base_path, from);
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
        let items = changes_to_items(dir.path(), &changes).unwrap();
        assert_eq!(items.len(), 2);

        // Sorted: /new.rs before /old.rs
        assert_eq!(items[0].path, "/new.rs");
        assert_eq!(items[0].ino, 5);
        assert_eq!(items[0].base_path, "");

        assert_eq!(items[1].path, "/old.rs");
        assert_eq!(items[1].ino, 0);
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
        let items = changes_to_items(dir.path(), &changes).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].path, "/a");
        assert_eq!(items[1].path, "/a/file.rs");
        assert_eq!(items[2].path, "/z/file.rs");
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
        let items = changes_to_items(dir.path(), &changes).unwrap();
        assert_eq!(items[0].d_type, libc::DT_DIR);
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
        let items = changes_to_items(dir.path(), &changes).unwrap();
        assert_eq!(items[0].d_type, libc::DT_LNK);
    }

    #[test]
    fn items_to_entries_sets_pointers() {
        let items = vec![RestoreItem {
            path: "/src/main.rs".into(),
            ino: 1,
            base_path: String::new(),
            d_type: libc::DT_REG,
        }];
        let entries = items_to_entries(&items).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path_len, 12);
        assert_eq!(entries[0].ino, 1);
        assert_eq!(entries[0].d_type, libc::DT_REG);
        assert_eq!(entries[0].base_path_len, 0);
    }

    #[test]
    fn items_to_entries_rejects_oversized_path() {
        let items = vec![RestoreItem {
            path: "a".repeat(u16::MAX as usize + 1),
            ino: 0,
            base_path: String::new(),
            d_type: 0,
        }];
        assert!(items_to_entries(&items).is_err());
    }

    #[test]
    fn items_to_entries_rejects_oversized_base_path() {
        let items = vec![RestoreItem {
            path: "/ok".into(),
            ino: 0,
            base_path: "a".repeat(u16::MAX as usize + 1),
            d_type: 0,
        }];
        assert!(items_to_entries(&items).is_err());
    }

    #[test]
    fn empty_changes_produces_no_entries() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("inodes")).unwrap();

        let items = changes_to_items(dir.path(), &[]).unwrap();
        assert!(items.is_empty());
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
        let items = changes_to_items(dir.path(), &changes).unwrap();

        // Find the "to" entry (the one with base_path set)
        let to_item = items.iter().find(|e| e.base_path == from).unwrap();
        assert_eq!(
            to_item.d_type,
            libc::DT_DIR,
            "renamed directory should have DT_DIR, got {}",
            to_item.d_type
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
        let items = changes_to_items(dir.path(), &changes).unwrap();

        let to_item = items.iter().find(|e| e.base_path == from).unwrap();
        assert_eq!(
            to_item.d_type,
            libc::DT_LNK,
            "renamed symlink should have DT_LNK, got {}",
            to_item.d_type
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
        let result = changes_to_items(dir.path(), &changes);
        assert!(result.is_err(), "missing inode should produce an error");
    }
}
