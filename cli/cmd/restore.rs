// agfs CLI — restore.rs
//
// `agfs restore <name|id>` — restore to a previous checkpoint.

use crate::ioctl;
use crate::journal::Journal;
use anyhow::{Context, Result};
use colored::Colorize;

pub fn run(checkpoint_name: &str) -> Result<()> {
    let agfs = crate::utils::session_dir()?;
    crate::utils::join_daemon_namespace(&agfs)?;

    // Search all markers (including dead zones) for the target checkpoint,
    // so that undo-restore (restoring to a dead checkpoint) works.
    let journal = Journal::read(&agfs)?;
    let (target_gen, chk_name) = journal.markers.find_checkpoint(checkpoint_name)?;
    let chk_label = chk_name.to_owned();

    // Extract live records from the prefix up to the target checkpoint,
    // handling any RST records within that prefix.
    let tree = journal.into_tree_at(target_gen);
    let count = tree.len();
    let buf = tree.serialize();

    // Restore kernel state — if this fails (e.g. EBUSY), the journal is
    // still intact (append-only) and the operation can be retried.
    let ctl_file = ioctl::open(&agfs).context("opening ctl for restore")?;
    let _new_gen = ioctl::restore(&ctl_file, target_gen, &buf).context("ioctl RESTORE")?;

    println!(
        "{}",
        format!(
            "Restored to checkpoint \"{chk_label}\" ({count} staged change{}).",
            crate::utils::plural(count)
        )
        .green()
        .bold()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::journal::{Action, DirTree, Dstate, Segment};

    fn build(actions: &[Action]) -> DirTree {
        DirTree::build(std::iter::once(Segment {
            from: 0,
            records: actions.to_vec(),
        }))
    }

    #[test]
    fn added_produces_single_entry() {
        let tree = build(&[Action::Add {
            path: "/src/main.rs".into(),
            ino: 1,
            dtype: Some(libc::DT_REG),
        }]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(
            tree.get("/src/main.rs"),
            Some(Dstate::StagedInode {
                ino: 1,
                in_base: false,
                ..
            })
        ));
    }

    #[test]
    fn deleted_produces_tombstone_entry() {
        let tree = build(&[
            Action::Modify {
                path: "/old.txt".into(),
                ino: 1,
                dtype: Some(libc::DT_REG),
            },
            Action::Delete {
                path: "/old.txt".into(),
                dtype: Some(libc::DT_REG),
            },
        ]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(
            tree.get("/old.txt"),
            Some(Dstate::Tombstone { .. })
        ));
    }

    #[test]
    fn renamed_produces_tombstone_and_redirect() {
        let tree = build(&[Action::Rename {
            src: "/a.txt".into(),
            dst: "/b.txt".into(),
            dtype: Some(libc::DT_REG),
        }]);

        assert!(matches!(tree.get("/a.txt"), Some(Dstate::Tombstone { .. })));
        assert!(
            matches!(tree.get("/b.txt"), Some(Dstate::Redirect { src, .. }) if src == "/a.txt")
        );
    }

    #[test]
    fn renamed_then_modified_produces_tombstone_and_inode() {
        let tree = build(&[
            Action::Rename {
                src: "/old.rs".into(),
                dst: "/new.rs".into(),
                dtype: Some(libc::DT_REG),
            },
            Action::Modify {
                path: "/new.rs".into(),
                ino: 5,
                dtype: Some(libc::DT_REG),
            },
        ]);

        assert!(matches!(
            tree.get("/new.rs"),
            Some(Dstate::StagedInode { ino: 5, .. })
        ));
        assert!(matches!(
            tree.get("/old.rs"),
            Some(Dstate::Tombstone { .. })
        ));
    }

    #[test]
    fn directory_inode_gets_dir_dtype() {
        let tree = build(&[Action::Add {
            path: "/newdir".into(),
            ino: 1,
            dtype: Some(libc::DT_DIR),
        }]);
        assert_eq!(tree.get("/newdir").unwrap().dtype(), libc::DT_DIR);
    }

    #[test]
    fn symlink_inode_gets_link_dtype() {
        let tree = build(&[Action::Add {
            path: "/link".into(),
            ino: 1,
            dtype: Some(libc::DT_LNK),
        }]);
        assert_eq!(tree.get("/link").unwrap().dtype(), libc::DT_LNK);
    }

    #[test]
    fn empty_records_produces_no_entries() {
        let tree = build(&[]);
        assert!(tree.is_empty());
    }

    #[test]
    fn renamed_directory_gets_dir_dtype() {
        let tree = build(&[Action::Rename {
            src: "/mydir".into(),
            dst: "/newdir".into(),
            dtype: Some(libc::DT_DIR),
        }]);
        assert_eq!(tree.get("/newdir").unwrap().dtype(), libc::DT_DIR);
    }

    #[test]
    fn renamed_symlink_gets_link_dtype() {
        let tree = build(&[Action::Rename {
            src: "/mylink".into(),
            dst: "/newlink".into(),
            dtype: Some(libc::DT_LNK),
        }]);
        assert_eq!(tree.get("/newlink").unwrap().dtype(), libc::DT_LNK);
    }
}
