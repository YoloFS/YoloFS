// agfs CLI — restore.rs
//
// `agfs restore <name|id>` — restore to a previous checkpoint.

use crate::{ioctl, journal, resolve};
use anyhow::{Context, Result};
use colored::Colorize;

/// Intermediate representation of a restore entry with owned path data.
struct RestoreItem {
    path: String,
    ino: u64,
    base: String,
    d_type: u8,
}

impl RestoreItem {
    fn deleted(path: String) -> Self {
        Self {
            path,
            ino: 0,
            base: String::new(),
            d_type: 0,
        }
    }
}

/// Convert a resolved Change list into restore items (owned data, sortable).
fn changes_to_items(changes: &[resolve::Change]) -> Vec<RestoreItem> {
    use std::collections::BTreeSet;

    // Collect destination paths that have staged content (Added/Modified).
    // When a Renamed destination also has a Modified entry, the staged inode
    // takes precedence over a redirect — skip the redundant redirect.
    let staged_paths: BTreeSet<&str> = changes
        .iter()
        .filter_map(|c| match c {
            resolve::Change::Added { path, .. } | resolve::Change::Modified { path, .. } => {
                Some(path.as_str())
            }
            _ => None,
        })
        .collect();

    let mut items = Vec::new();

    for change in changes {
        match change {
            resolve::Change::Added { path, ino, dtype }
            | resolve::Change::Modified { path, ino, dtype } => {
                items.push(RestoreItem {
                    path: path.clone(),
                    ino: *ino,
                    base: String::new(),
                    d_type: dtype.to_libc(),
                });
            }
            resolve::Change::Deleted(path) => {
                items.push(RestoreItem::deleted(path.clone()));
            }
            resolve::Change::Renamed { from, to, dtype } => {
                items.push(RestoreItem::deleted(from.clone()));
                if !staged_paths.contains(to.as_str()) {
                    items.push(RestoreItem {
                        path: to.clone(),
                        ino: journal::INO_REDIRECT,
                        base: from.clone(),
                        d_type: dtype.to_libc(),
                    });
                }
            }
        }
    }

    items.sort_by(|a, b| a.path.cmp(&b.path));
    items
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
            let base_len: u16 = item
                .base
                .len()
                .try_into()
                .context("restore base too long")?;
            Ok(ioctl::AgfsIocRestoreEntry {
                path_ptr: item.path.as_ptr() as u64,
                path_len,
                d_type: item.d_type,
                _pad1: [0u8; 5],
                ino: item.ino,
                base_ptr: item.base.as_ptr() as u64,
                base_len,
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
        journal::Record::Checkpoint { id, name } => (*id, name.clone()),
        _ => unreachable!("find_checkpoint_index returned non-checkpoint record"),
    };

    let truncate_offset = journal
        .offset(chk_idx)
        .context("checkpoint offset out of bounds")?;

    let mut records = journal.records;
    records.truncate(chk_idx + 1);
    let changes = resolve::resolve(records)?;
    let items = changes_to_items(&changes);
    let entries = items_to_entries(&items)?;

    // Restore kernel state first — if this fails (e.g. EBUSY), the
    // journal is still intact and the operation can be retried.
    let ctl_file = ioctl::open(&agfs).context("opening ctl for restore")?;
    ioctl::restore(&ctl_file, checkpoint_gen, &entries).context("ioctl RESTORE")?;

    // Truncate journal after the checkpoint record — preserves the inode
    // so the kernel's O_APPEND fd stays valid.
    journal::truncate(&agfs, truncate_offset)?;

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
    use crate::journal::DType;
    use crate::resolve::Change;

    #[test]
    fn added_produces_single_entry() {
        let changes = vec![Change::Added {
            path: "/src/main.rs".into(),
            ino: 1,
            dtype: DType::File,
        }];
        let items = changes_to_items(&changes);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, "/src/main.rs");
        assert_eq!(items[0].ino, 1);
        assert_eq!(items[0].d_type, libc::DT_REG);
        assert_eq!(items[0].base, "");
    }

    #[test]
    fn deleted_produces_zero_entry() {
        let changes = vec![Change::Deleted("/old.txt".into())];
        let items = changes_to_items(&changes);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, "/old.txt");
        assert_eq!(items[0].ino, 0);
        assert_eq!(items[0].base, "");
    }

    #[test]
    fn renamed_produces_delete_and_redirect() {
        let changes = vec![Change::Renamed {
            from: "/a.txt".into(),
            to: "/b.txt".into(),
            dtype: DType::File,
        }];
        let items = changes_to_items(&changes);
        assert_eq!(items.len(), 2);

        let del = items.iter().find(|e| e.path == "/a.txt").unwrap();
        assert_eq!(del.ino, 0);

        let redirect = items.iter().find(|e| e.path == "/b.txt").unwrap();
        assert_eq!(redirect.ino, journal::INO_REDIRECT);
        assert_eq!(redirect.base, "/a.txt");
        assert_eq!(redirect.d_type, libc::DT_REG);
    }

    #[test]
    fn renamed_modified_produces_delete_and_ino() {
        let changes = vec![
            Change::Renamed {
                from: "/old.rs".into(),
                to: "/new.rs".into(),
                dtype: DType::File,
            },
            Change::Modified {
                path: "/new.rs".into(),
                ino: 5,
                dtype: DType::File,
            },
        ];
        let items = changes_to_items(&changes);
        assert_eq!(items.len(), 2);

        // Sorted: /new.rs before /old.rs
        assert_eq!(items[0].path, "/new.rs");
        assert_eq!(items[0].ino, 5);
        assert_eq!(items[0].base, "");

        assert_eq!(items[1].path, "/old.rs");
        assert_eq!(items[1].ino, 0);
    }

    #[test]
    fn entries_sorted_by_path() {
        let changes = vec![
            Change::Added {
                path: "/z/file.rs".into(),
                ino: 1,
                dtype: DType::File,
            },
            Change::Added {
                path: "/a/file.rs".into(),
                ino: 2,
                dtype: DType::File,
            },
            Change::Added {
                path: "/a".into(),
                ino: 3,
                dtype: DType::Dir,
            },
        ];
        let items = changes_to_items(&changes);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].path, "/a");
        assert_eq!(items[1].path, "/a/file.rs");
        assert_eq!(items[2].path, "/z/file.rs");
    }

    #[test]
    fn directory_inode_gets_dt_dir() {
        let changes = vec![Change::Added {
            path: "/newdir".into(),
            ino: 1,
            dtype: DType::Dir,
        }];
        let items = changes_to_items(&changes);
        assert_eq!(items[0].d_type, libc::DT_DIR);
    }

    #[test]
    fn symlink_inode_gets_dt_lnk() {
        let changes = vec![Change::Added {
            path: "/link".into(),
            ino: 1,
            dtype: DType::Link,
        }];
        let items = changes_to_items(&changes);
        assert_eq!(items[0].d_type, libc::DT_LNK);
    }

    #[test]
    fn items_to_entries_sets_pointers() {
        let items = vec![RestoreItem {
            path: "/src/main.rs".into(),
            ino: 1,
            base: String::new(),
            d_type: libc::DT_REG,
        }];
        let entries = items_to_entries(&items).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path_len, 12);
        assert_eq!(entries[0].ino, 1);
        assert_eq!(entries[0].d_type, libc::DT_REG);
        assert_eq!(entries[0].base_len, 0);
    }

    #[test]
    fn items_to_entries_rejects_oversized_path() {
        let items = vec![RestoreItem {
            path: "a".repeat(u16::MAX as usize + 1),
            ino: 0,
            base: String::new(),
            d_type: 0,
        }];
        assert!(items_to_entries(&items).is_err());
    }

    #[test]
    fn items_to_entries_rejects_oversized_base() {
        let items = vec![RestoreItem {
            path: "/ok".into(),
            ino: 0,
            base: "a".repeat(u16::MAX as usize + 1),
            d_type: 0,
        }];
        assert!(items_to_entries(&items).is_err());
    }

    #[test]
    fn empty_changes_produces_no_entries() {
        let items = changes_to_items(&[]);
        assert!(items.is_empty());
    }

    /// Renamed directory must produce DT_DIR, not DT_REG.
    #[test]
    fn renamed_directory_gets_dt_dir() {
        let changes = vec![Change::Renamed {
            from: "/mydir".into(),
            to: "/newdir".into(),
            dtype: DType::Dir,
        }];
        let items = changes_to_items(&changes);

        let to_item = items.iter().find(|e| e.path == "/newdir").unwrap();
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
        let changes = vec![Change::Renamed {
            from: "/mylink".into(),
            to: "/newlink".into(),
            dtype: DType::Link,
        }];
        let items = changes_to_items(&changes);

        let to_item = items.iter().find(|e| e.path == "/newlink").unwrap();
        assert_eq!(
            to_item.d_type,
            libc::DT_LNK,
            "renamed symlink should have DT_LNK, got {}",
            to_item.d_type
        );
    }
}
