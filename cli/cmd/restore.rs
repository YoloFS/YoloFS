// agfs CLI — restore.rs
//
// `agfs restore <name|id>` — restore to a previous checkpoint.

use crate::ioctl;
use crate::journal::{Dirent, Journal, INO_REDIRECT};
use anyhow::{Context, Result};
use colored::Colorize;

/// Convert (path, Dirent) pairs into ioctl entries with pointers into the source data.
/// The returned entries are valid as long as `dirents` is alive.
fn dirents_to_entries(
    dirents: &[(String, Dirent)],
) -> Result<Vec<ioctl::AgfsIocRestoreEntry>> {
    dirents
        .iter()
        .map(|(path, dirent)| {
            let path_len: u16 = path
                .len()
                .try_into()
                .context("restore path too long")?;

            let (ino, d_type, in_base, base_ptr, base_len) = match dirent {
                Dirent::Inode {
                    ino, dtype, in_base,
                } => (*ino, dtype.to_libc(), *in_base as u8, path.as_ptr() as u64, 0u16),
                Dirent::Link {
                    base_path,
                    dtype,
                    in_base,
                } => {
                    let blen: u16 = base_path
                        .len()
                        .try_into()
                        .context("restore base too long")?;
                    (INO_REDIRECT, dtype.to_libc(), *in_base as u8, base_path.as_ptr() as u64, blen)
                }
                Dirent::Tombstone { dtype } => {
                    (0, dtype.to_libc(), 1u8, path.as_ptr() as u64, 0u16)
                }
            };

            Ok(ioctl::AgfsIocRestoreEntry {
                path_ptr: path.as_ptr() as u64,
                path_len,
                d_type,
                in_base,
                _pad1: [0u8; 4],
                ino,
                base_ptr,
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
    let journal = Journal::read(&agfs)?;
    let (target_gen, chk_name) = journal.markers.find_checkpoint(checkpoint_name)?;
    let chk_label = chk_name.to_owned();

    // Extract live records from the prefix up to the target checkpoint,
    // handling any RST records within that prefix.
    let dirents = journal.into_tree_at(target_gen).into_dirents();
    let entries = dirents_to_entries(&dirents)?;

    // Restore kernel state — if this fails (e.g. EBUSY), the journal is
    // still intact (append-only) and the operation can be retried.
    let ctl_file = ioctl::open(&agfs).context("opening ctl for restore")?;
    let _new_gen = ioctl::restore(&ctl_file, target_gen, &entries).context("ioctl RESTORE")?;

    println!(
        "{}",
        format!(
            "Restored to checkpoint \"{chk_label}\" ({} staged change{}).",
            dirents.len(),
            crate::utils::plural(dirents.len())
        )
        .green()
        .bold()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Action, DType, DirTree, Segment};

    /// Helper: build a tree from actions and get dirents.
    fn build_dirents(actions: &[Action]) -> Vec<(String, Dirent)> {
        DirTree::build(std::iter::once(Segment { from: 0, records: actions.to_vec() })).into_dirents()
    }

    /// Helper: find a dirent by path suffix.
    fn find<'a>(cs: &'a [(String, Dirent)], suffix: &str) -> &'a (String, Dirent) {
        cs.iter().find(|(p, _)| p.ends_with(suffix)).unwrap()
    }

    #[test]
    fn added_produces_single_entry() {
        let cs = build_dirents(&[Action::Add {
            path: "/src/main.rs".into(),
            ino: 1,
            dtype: Some(DType::File),
        }]);
        assert_eq!(cs.len(), 1);
        let (path, dirent) = &cs[0];
        assert_eq!(path, "/src/main.rs");
        assert!(matches!(dirent, Dirent::Inode { ino: 1, in_base: false, .. }));
    }

    #[test]
    fn deleted_produces_tombstone_entry() {
        let cs = build_dirents(&[
            Action::Modify {
                path: "/old.txt".into(),
                ino: 1,
                dtype: Some(DType::File),
            },
            Action::Delete {
                path: "/old.txt".into(),
                dtype: Some(DType::File),
            },
        ]);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].0, "/old.txt");
        assert!(matches!(cs[0].1, Dirent::Tombstone { .. }));
    }

    #[test]
    fn renamed_produces_tombstone_and_redirect() {
        let cs = build_dirents(&[Action::Rename {
            src: "/a.txt".into(),
            dst: "/b.txt".into(),
            dtype: Some(DType::File),
        }]);

        let (_, del) = find(&cs, "/a.txt");
        assert!(matches!(del, Dirent::Tombstone { .. }));

        let (_, redirect) = find(&cs, "/b.txt");
        assert!(matches!(redirect, Dirent::Link { base_path, .. } if base_path == "/a.txt"));
    }

    #[test]
    fn renamed_then_modified_produces_tombstone_and_inode() {
        let cs = build_dirents(&[
            Action::Rename {
                src: "/old.rs".into(),
                dst: "/new.rs".into(),
                dtype: Some(DType::File),
            },
            Action::Modify {
                path: "/new.rs".into(),
                ino: 5,
                dtype: Some(DType::File),
            },
        ]);

        let (_, new) = find(&cs, "/new.rs");
        assert!(matches!(new, Dirent::Inode { ino: 5, .. }));

        let (_, old) = find(&cs, "/old.rs");
        assert!(matches!(old, Dirent::Tombstone { .. }));
    }

    #[test]
    fn directory_inode_gets_dir_dtype() {
        let cs = build_dirents(&[Action::Add {
            path: "/newdir".into(),
            ino: 1,
            dtype: Some(DType::Dir),
        }]);
        assert_eq!(cs[0].1.dtype(), DType::Dir);
    }

    #[test]
    fn symlink_inode_gets_link_dtype() {
        let cs = build_dirents(&[Action::Add {
            path: "/link".into(),
            ino: 1,
            dtype: Some(DType::Link),
        }]);
        assert_eq!(cs[0].1.dtype(), DType::Link);
    }

    #[test]
    fn dirents_to_entries_sets_pointers() {
        let cs = vec![(
            "/src/main.rs".to_string(),
            Dirent::Inode {
                ino: 1,
                dtype: DType::File,
                in_base: false,
            },
        )];
        let entries = dirents_to_entries(&cs).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path_len, 12);
        assert_eq!(entries[0].ino, 1);
        assert_eq!(entries[0].d_type, libc::DT_REG);
        assert_eq!(entries[0].base_len, 0);
    }

    #[test]
    fn dirents_to_entries_rejects_oversized_path() {
        let cs = vec![(
            "a".repeat(u16::MAX as usize + 1),
            Dirent::Tombstone { dtype: DType::File },
        )];
        assert!(dirents_to_entries(&cs).is_err());
    }

    #[test]
    fn dirents_to_entries_rejects_oversized_base() {
        let cs = vec![(
            "/ok".into(),
            Dirent::Link {
                base_path: "a".repeat(u16::MAX as usize + 1),
                dtype: DType::File,
                in_base: false,
            },
        )];
        assert!(dirents_to_entries(&cs).is_err());
    }

    #[test]
    fn empty_records_produces_no_entries() {
        let cs = build_dirents(&[]);
        assert!(cs.is_empty());
    }

    #[test]
    fn renamed_directory_gets_dir_dtype() {
        let cs = build_dirents(&[Action::Rename {
            src: "/mydir".into(),
            dst: "/newdir".into(),
            dtype: Some(DType::Dir),
        }]);
        let (_, dirent) = find(&cs, "/newdir");
        assert_eq!(dirent.dtype(), DType::Dir);
    }

    #[test]
    fn renamed_symlink_gets_link_dtype() {
        let cs = build_dirents(&[Action::Rename {
            src: "/mylink".into(),
            dst: "/newlink".into(),
            dtype: Some(DType::Link),
        }]);
        let (_, dirent) = find(&cs, "/newlink");
        assert_eq!(dirent.dtype(), DType::Link);
    }
}
