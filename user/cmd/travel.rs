// yolo CLI — travel.rs
//
// `yolo travel <gen>` — travel to the marker with that generation id.

use crate::ioctl;
use crate::journal::{Journal, Marker};
use crate::report;
use anyhow::{Context, Result};

pub fn run(gen_arg: &str) -> Result<()> {
    let yolofs = crate::utils::session_dir()?;

    let gen_id = crate::utils::parse_gen(gen_arg)?;

    // Resolve against all markers (including dead zones) so undo-travel
    // (traveling to a dead marker) works.
    let journal = Journal::read(&yolofs)?;
    let target_gen = journal.markers.resolve_gen(gen_id)?;
    let marker = journal.markers.get(target_gen as usize).cloned();

    // Extract live records from the prefix up to the target marker,
    // handling any T records within that prefix.
    let tree = journal.into_tree_at(target_gen);
    let count = tree.len();
    let buf = tree.serialize();

    // Travel kernel state — if this fails (e.g. EBUSY), the journal is
    // still intact (append-only) and the operation can be retried.
    let ctl_file = ioctl::open(&yolofs).context("opening ctl for travel")?;
    let _new_gen = ioctl::travel(&ctl_file, target_gen, &buf).context("ioctl TRAVEL")?;

    let label = match &marker {
        Some(Marker::Snapshot { name }) => {
            format!("snapshot \"{name}\"")
        }
        // The resolved `target_gen` is this marker's own index (gen); `dest` is
        // where that travel marker points.
        Some(Marker::Travel { target_gen: dest }) => {
            format!("travel {target_gen} → {dest}")
        }
        None => format!("marker {target_gen}"),
    };

    report::success(format!(
        "traveled to {label} ({count} staged change{})",
        crate::utils::plural(count)
    ));

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::journal::{Action, DirTree, Record, Segment, Target};

    fn build(actions: &[Action]) -> DirTree {
        DirTree::build(std::iter::once(Segment {
            records: actions.iter().cloned().map(Record::Action).collect(),
        }))
    }

    #[test]
    fn added_produces_single_entry() {
        let tree = build(&[Action::Stage {
            path: "/src/main.rs".into(),
            ino: 1,
            pre: Target::Absence,
        }]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(
            tree.get("/src/main.rs"),
            Some(Target::StagedFile(1))
        ));
    }

    #[test]
    fn deleted_produces_tombstone_entry() {
        let tree = build(&[
            Action::Stage {
                path: "/old.txt".into(),
                ino: 1,
                pre: Target::Absence,
            },
            Action::Delete {
                path: "/old.txt".into(),
                pre: Target::Absence,
            },
        ]);
        assert_eq!(tree.len(), 1);
        assert!(matches!(tree.get("/old.txt"), Some(Target::Absence)));
    }

    #[test]
    fn renamed_produces_tombstone_and_redirect() {
        let tree = build(&[Action::Rename {
            src: "/a.txt".into(),
            dst: "/b.txt".into(),
            src_pre: Target::BasePath("/a.txt".into()),
            dst_pre: Target::Absence,
        }]);

        assert!(matches!(tree.get("/a.txt"), Some(Target::Absence)));
        assert!(matches!(tree.get("/b.txt"), Some(Target::BasePath(src)) if src == "/a.txt"));
    }

    #[test]
    fn renamed_then_modified_produces_tombstone_and_inode() {
        let tree = build(&[
            Action::Rename {
                src: "/old.rs".into(),
                dst: "/new.rs".into(),
                src_pre: Target::BasePath("/old.rs".into()),
                dst_pre: Target::Absence,
            },
            Action::Stage {
                path: "/new.rs".into(),
                ino: 5,
                pre: Target::Absence,
            },
        ]);

        assert!(matches!(tree.get("/new.rs"), Some(Target::StagedFile(5))));
        assert!(matches!(tree.get("/old.rs"), Some(Target::Absence)));
    }

    #[test]
    fn directory_inode_gets_node() {
        let tree = build(&[Action::Stage {
            path: "/newdir".into(),
            ino: 1,
            pre: Target::Absence,
        }]);
        let node = tree.get_node("/newdir").expect("should exist");
        assert!(matches!(node.new, Some(Target::StagedFile(1))));
    }

    #[test]
    fn symlink_inode_gets_node() {
        let tree = build(&[Action::Stage {
            path: "/link".into(),
            ino: 1,
            pre: Target::Absence,
        }]);
        let node = tree.get_node("/link").expect("should exist");
        assert!(matches!(node.new, Some(Target::StagedFile(1))));
    }

    #[test]
    fn empty_records_produces_no_entries() {
        let tree = build(&[]);
        assert!(tree.is_empty());
    }

    #[test]
    fn renamed_directory_gets_node() {
        let tree = build(&[Action::Rename {
            src: "/mydir".into(),
            dst: "/newdir".into(),
            src_pre: Target::BasePath("/mydir".into()),
            dst_pre: Target::Absence,
        }]);
        let node = tree.get_node("/newdir").expect("should exist");
        assert!(matches!(node.new, Some(Target::BasePath(ref src)) if src == "/mydir"));
    }

    #[test]
    fn renamed_symlink_gets_node() {
        let tree = build(&[Action::Rename {
            src: "/mylink".into(),
            dst: "/newlink".into(),
            src_pre: Target::BasePath("/mylink".into()),
            dst_pre: Target::Absence,
        }]);
        let node = tree.get_node("/newlink").expect("should exist");
        assert!(matches!(node.new, Some(Target::BasePath(ref src)) if src == "/mylink"));
    }
}
