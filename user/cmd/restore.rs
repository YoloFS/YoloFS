// agfs CLI — restore.rs
//
// `agfs restore <name|id>` — restore to a previous meta.

use crate::ioctl;
use crate::journal::{Journal, Meta};
use anyhow::{Context, Result};
use colored::Colorize;

pub fn run(meta_name: &str) -> Result<()> {
    let agfs = crate::utils::session_dir()?;

    // Search all metas (including dead zones) for the target,
    // so that undo-restore (restoring to a dead meta) works.
    let journal = Journal::read(&agfs)?;
    let target_gen = journal.metas.find_meta(meta_name)?;
    let meta = journal.metas.get(target_gen as usize).cloned();

    // Extract live records from the prefix up to the target meta,
    // handling any J records within that prefix.
    let tree = journal.into_tree_at(target_gen);
    let count = tree.len();
    let buf = tree.serialize();

    // Jump kernel state — if this fails (e.g. EBUSY), the journal is
    // still intact (append-only) and the operation can be retried.
    let ctl_file = ioctl::open(&agfs).context("opening ctl for jump")?;
    let _new_gen = ioctl::jump(&ctl_file, target_gen, &buf).context("ioctl JUMP")?;

    let label = match &meta {
        Some(Meta::Mark { name, .. }) => {
            format!("checkpoint \"{name}\"")
        }
        Some(Meta::Jump {
            gen_id, target_gen, ..
        }) => {
            format!("restore [{gen_id}] (restored to [{target_gen}])")
        }
        None => format!("meta [{target_gen}]"),
    };

    println!(
        "{}",
        format!(
            "Restored to {label} ({count} staged change{}).",
            crate::utils::plural(count)
        )
        .green()
        .bold()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::journal::{Action, DirNode, DirTree, Segment, Target};

    fn build(actions: &[Action]) -> DirTree {
        DirTree::build(std::iter::once(Segment {
            from: 0,
            records: actions.to_vec(),
        }))
    }

    #[test]
    fn added_produces_single_entry() {
        let tree = build(&[Action::Stage {
            path: "/src/main.rs".into(),
            ino: 1,
            dtype: Some(libc::DT_REG),
        }]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/src/main.rs"), Some(Target::Inode(1))));
    }

    #[test]
    fn deleted_produces_tombstone_entry() {
        let tree = build(&[
            Action::Stage {
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
        assert!(matches!(tree.get("/old.txt"), Some(Target::None)));
    }

    #[test]
    fn renamed_produces_tombstone_and_redirect() {
        let tree = build(&[Action::Rename {
            src: "/a.txt".into(),
            dst: "/b.txt".into(),
            dtype: Some(libc::DT_REG),
        }]);

        assert!(matches!(tree.get("/a.txt"), Some(Target::None)));
        assert!(matches!(tree.get("/b.txt"), Some(Target::Path(Some(src))) if src == "/a.txt"));
    }

    #[test]
    fn renamed_then_modified_produces_tombstone_and_inode() {
        let tree = build(&[
            Action::Rename {
                src: "/old.rs".into(),
                dst: "/new.rs".into(),
                dtype: Some(libc::DT_REG),
            },
            Action::Stage {
                path: "/new.rs".into(),
                ino: 5,
                dtype: Some(libc::DT_REG),
            },
        ]);

        assert!(matches!(tree.get("/new.rs"), Some(Target::Inode(5))));
        assert!(matches!(tree.get("/old.rs"), Some(Target::None)));
    }

    #[test]
    fn directory_inode_gets_dir_node() {
        let tree = build(&[Action::Stage {
            path: "/newdir".into(),
            ino: 1,
            dtype: Some(libc::DT_DIR),
        }]);
        assert!(matches!(tree.get_node("/newdir"), Some(DirNode::Dir(_, _))));
    }

    #[test]
    fn symlink_inode_gets_file_node() {
        let tree = build(&[Action::Stage {
            path: "/link".into(),
            ino: 1,
            dtype: Some(libc::DT_LNK),
        }]);
        // Symlinks are stored as File nodes (leaf)
        assert!(matches!(tree.get_node("/link"), Some(DirNode::File(_))));
    }

    #[test]
    fn empty_records_produces_no_entries() {
        let tree = build(&[]);
        assert!(tree.is_empty());
    }

    #[test]
    fn renamed_directory_gets_dir_node() {
        let tree = build(&[Action::Rename {
            src: "/mydir".into(),
            dst: "/newdir".into(),
            dtype: Some(libc::DT_DIR),
        }]);
        assert!(matches!(tree.get_node("/newdir"), Some(DirNode::Dir(_, _))));
    }

    #[test]
    fn renamed_symlink_gets_file_node() {
        let tree = build(&[Action::Rename {
            src: "/mylink".into(),
            dst: "/newlink".into(),
            dtype: Some(libc::DT_LNK),
        }]);
        // Symlinks are stored as File nodes (leaf)
        assert!(matches!(tree.get_node("/newlink"), Some(DirNode::File(_))));
    }
}
