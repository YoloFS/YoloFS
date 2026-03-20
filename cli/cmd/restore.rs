// agfs CLI — restore.rs
//
// `agfs restore <name|id>` — restore to a previous checkpoint.

use crate::journal::SegmentedJournal;
use crate::{ioctl, journal};
use anyhow::{Context, Result};
use colored::Colorize;

/// Intermediate representation of a restore entry with owned path data.
struct RestoreItem {
    path: String,
    ino: u64,
    base: String,
    overwrites: bool,
    d_type: u8,
}

impl RestoreItem {
    fn deleted(path: String) -> Self {
        Self {
            path,
            ino: 0,
            base: String::new(),
            overwrites: true,
            d_type: 0,
        }
    }
}

/// Convert a resolved Change list into restore items (owned data, sortable).
fn changes_to_items(changes: &[(String, journal::Change)]) -> Vec<RestoreItem> {
    use std::collections::BTreeSet;

    // Collect destination paths that have staged content (Added/Modified).
    // When a Renamed destination also has a Modified entry, the staged inode
    // takes precedence over a redirect — skip the redundant redirect.
    let staged_paths: BTreeSet<&str> = changes
        .iter()
        .filter_map(|(path, c)| match c {
            journal::Change::Added { .. } | journal::Change::Modified { .. } => Some(path.as_str()),
            _ => None,
        })
        .collect();

    let mut items = Vec::new();

    for (path, change) in changes {
        match change {
            journal::Change::Added { ino, dtype } | journal::Change::Modified { ino, dtype } => {
                let overwrites = matches!(change, journal::Change::Modified { .. });
                items.push(RestoreItem {
                    path: path.clone(),
                    ino: *ino,
                    base: String::new(),
                    overwrites,
                    d_type: dtype.to_libc(),
                });
            }
            journal::Change::Deleted => {
                items.push(RestoreItem::deleted(path.clone()));
            }
            journal::Change::Renamed { from, dtype } => {
                items.push(RestoreItem::deleted(from.clone()));
                if !staged_paths.contains(path.as_str()) {
                    items.push(RestoreItem {
                        path: path.clone(),
                        ino: journal::INO_REDIRECT,
                        base: from.clone(),
                        overwrites: false,
                        d_type: dtype.to_libc(),
                    });
                }
            }
            journal::Change::Replaced { from, dtype } => {
                items.push(RestoreItem::deleted(from.clone()));
                if !staged_paths.contains(path.as_str()) {
                    items.push(RestoreItem {
                        path: path.clone(),
                        ino: journal::INO_REDIRECT,
                        base: from.clone(),
                        overwrites: true,
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
                overwrites: item.overwrites as u8,
                _pad1: [0u8; 4],
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

    // Search all markers (including dead zones) for the target checkpoint,
    // so that undo-restore (restoring to a dead checkpoint) works.
    let sj = SegmentedJournal::new(journal::read(&agfs)?);
    let (target_gen, chk_name) = sj.markers.find_checkpoint(checkpoint_name)?;
    let chk_label = chk_name.to_owned();

    // Extract live records from the prefix up to the target checkpoint,
    // handling any S records within that prefix.
    let live_records = sj.live_prefix_gen(target_gen).into_records();
    let actions = journal::compact::compact(live_records);
    let changes = actions.collapse();
    let items = changes_to_items(&changes.0);
    let entries = items_to_entries(&items)?;

    // Restore kernel state — if this fails (e.g. EBUSY), the journal is
    // still intact (append-only) and the operation can be retried.
    let ctl_file = ioctl::open(&agfs).context("opening ctl for restore")?;
    let _new_gen = ioctl::restore(&ctl_file, target_gen, &entries).context("ioctl RESTORE")?;

    println!(
        "{}",
        format!(
            "Restored to checkpoint \"{chk_label}\" ({} staged change{}).",
            changes.0.len(),
            crate::utils::plural(changes.0.len())
        )
        .green()
        .bold()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Change;
    use crate::journal::DType;

    #[test]
    fn added_produces_single_entry() {
        let changes = vec![(
            "/src/main.rs".into(),
            Change::Added {
                ino: 1,
                dtype: DType::File,
            },
        )];
        let items = changes_to_items(&changes);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, "/src/main.rs");
        assert_eq!(items[0].ino, 1);
        assert_eq!(items[0].d_type, libc::DT_REG);
        assert_eq!(items[0].base, "");
    }

    #[test]
    fn deleted_produces_zero_entry() {
        let changes = vec![("/old.txt".into(), Change::Deleted)];
        let items = changes_to_items(&changes);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, "/old.txt");
        assert_eq!(items[0].ino, 0);
        assert_eq!(items[0].base, "");
    }

    #[test]
    fn renamed_produces_delete_and_redirect() {
        let changes = vec![(
            "/b.txt".into(),
            Change::Renamed {
                from: "/a.txt".into(),
                dtype: DType::File,
            },
        )];
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
            (
                "/new.rs".into(),
                Change::Renamed {
                    from: "/old.rs".into(),
                    dtype: DType::File,
                },
            ),
            (
                "/new.rs".into(),
                Change::Modified {
                    ino: 5,
                    dtype: DType::File,
                },
            ),
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
            (
                "/z/file.rs".into(),
                Change::Added {
                    ino: 1,
                    dtype: DType::File,
                },
            ),
            (
                "/a/file.rs".into(),
                Change::Added {
                    ino: 2,
                    dtype: DType::File,
                },
            ),
            (
                "/a".into(),
                Change::Added {
                    ino: 3,
                    dtype: DType::Dir,
                },
            ),
        ];
        let items = changes_to_items(&changes);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].path, "/a");
        assert_eq!(items[1].path, "/a/file.rs");
        assert_eq!(items[2].path, "/z/file.rs");
    }

    #[test]
    fn directory_inode_gets_dt_dir() {
        let changes = vec![(
            "/newdir".into(),
            Change::Added {
                ino: 1,
                dtype: DType::Dir,
            },
        )];
        let items = changes_to_items(&changes);
        assert_eq!(items[0].d_type, libc::DT_DIR);
    }

    #[test]
    fn symlink_inode_gets_dt_lnk() {
        let changes = vec![(
            "/link".into(),
            Change::Added {
                ino: 1,
                dtype: DType::Link,
            },
        )];
        let items = changes_to_items(&changes);
        assert_eq!(items[0].d_type, libc::DT_LNK);
    }

    #[test]
    fn items_to_entries_sets_pointers() {
        let items = vec![RestoreItem {
            path: "/src/main.rs".into(),
            ino: 1,
            base: String::new(),
            overwrites: false,
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
            overwrites: false,
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
            overwrites: false,
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
        let changes = vec![(
            "/newdir".into(),
            Change::Renamed {
                from: "/mydir".into(),
                dtype: DType::Dir,
            },
        )];
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
        let changes = vec![(
            "/newlink".into(),
            Change::Renamed {
                from: "/mylink".into(),
                dtype: DType::Link,
            },
        )];
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
