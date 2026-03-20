// agfs CLI — journal/action.rs
//
// ActionList methods: apply() to base filesystem, collapse() to Changeset.

use super::types::{Action, ActionList, Change, Changeset, DType};
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Create parent directories for a path, skipping if already ensured.
fn ensure_parent(path: &Path, cache: &mut HashSet<PathBuf>) -> Result<()> {
    if let Some(parent) = path.parent()
        && !cache.contains(parent)
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating parent dirs for {}", path.display()))?;
        cache.insert(parent.to_path_buf());
    }
    Ok(())
}

/// Apply a staged inode to base. Stats the inode to determine type.
fn apply_inode(
    agfs_dir: &Path,
    ino: u64,
    base_path: &Path,
    ensured: &mut HashSet<PathBuf>,
) -> Result<()> {
    let staged = crate::utils::inode_path(agfs_dir, ino);
    let meta = fs::symlink_metadata(&staged)
        .with_context(|| format!("stat staged inode {}", staged.display()))?;

    ensure_parent(base_path, ensured)?;

    // Save existing file's permissions before removal so we can restore
    // them after moving the staged inode (preserves base file modes).
    let original_perms = base_path
        .symlink_metadata()
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.permissions());

    // Remove whatever exists at the target path
    if let Ok(existing) = base_path.symlink_metadata() {
        if existing.is_dir() && !existing.file_type().is_symlink() {
            fs::remove_dir_all(base_path)
                .with_context(|| format!("removing existing dir {}", base_path.display()))?;
        } else {
            fs::remove_file(base_path)
                .with_context(|| format!("removing existing file {}", base_path.display()))?;
        }
    }

    if meta.file_type().is_symlink() {
        let target = fs::read_link(&staged)?;
        std::os::unix::fs::symlink(&target, base_path)
            .with_context(|| format!("creating symlink at {}", base_path.display()))?;
    } else if meta.is_dir() {
        fs::create_dir_all(base_path).with_context(|| format!("mkdir {}", base_path.display()))?;
    } else {
        fs::rename(&staged, base_path)
            .or_else(|_| {
                fs::copy(&staged, base_path)?;
                fs::remove_file(&staged)?;
                Ok::<_, std::io::Error>(())
            })
            .with_context(|| format!("moving inode to {}", base_path.display()))?;
    }

    // Restore original permissions for modified regular files.
    if let Some(perms) = original_perms
        && meta.is_file()
        && !meta.file_type().is_symlink()
    {
        fs::set_permissions(base_path, perms)
            .with_context(|| format!("restoring permissions on {}", base_path.display()))?;
    }

    Ok(())
}

impl ActionList {
    /// Apply actions sequentially to the base filesystem.
    pub fn apply(&self, agfs: &Path) -> Result<()> {
        let mut ensured: HashSet<PathBuf> = HashSet::new();

        for action in &self.0 {
            match action {
                Action::Add { path, ino, .. } | Action::Modify { path, ino, .. } => {
                    let base_file = crate::utils::to_base_path(path);
                    apply_inode(agfs, *ino, &base_file, &mut ensured)?;
                }
                Action::Delete { path } => {
                    let base_file = crate::utils::to_base_path(path);
                    if let Ok(meta) = base_file.symlink_metadata() {
                        if meta.is_dir() && !meta.file_type().is_symlink() {
                            fs::remove_dir_all(&base_file)
                        } else {
                            fs::remove_file(&base_file)
                        }
                        .with_context(|| format!("deleting {path}"))?;
                    }
                }
                Action::Rename { old, new, .. } | Action::Replace { old, new, .. } => {
                    let base_old = crate::utils::to_base_path(old);
                    let base_new = crate::utils::to_base_path(new);
                    ensure_parent(&base_new, &mut ensured)?;
                    fs::rename(&base_old, &base_new)
                        .with_context(|| format!("rename {old} → {new}"))?;
                }
            }
        }

        Ok(())
    }

    /// Derive the state summary for display commands.
    pub fn collapse(&self) -> Changeset {
        let mut state: HashMap<String, Change> = HashMap::new();

        for action in &self.0 {
            match action {
                Action::Add { path, ino, dtype } => {
                    state.insert(
                        path.clone(),
                        Change::Added {
                            ino: *ino,
                            dtype: *dtype,
                        },
                    );
                }
                Action::Modify { path, ino, dtype } => {
                    state.insert(
                        path.clone(),
                        Change::Modified {
                            ino: *ino,
                            dtype: *dtype,
                        },
                    );
                }
                Action::Delete { path } => {
                    match state.remove(path) {
                        Some(Change::Added { .. }) => {
                            // Cancel: added then deleted — no net effect.
                        }
                        Some(Change::Renamed { from: origin, .. }) => {
                            state.entry(origin).or_insert(Change::Deleted);
                            state.insert(path.clone(), Change::Deleted);
                        }
                        Some(Change::Replaced { from: origin, .. }) => {
                            state.entry(origin).or_insert(Change::Deleted);
                            state.insert(path.clone(), Change::Deleted);
                        }
                        _ => {
                            state.insert(path.clone(), Change::Deleted);
                        }
                    }
                }
                Action::Rename { old, new, dtype } => {
                    collapse_rename(&mut state, old, new, *dtype, false);
                }
                Action::Replace { old, new, dtype } => {
                    collapse_rename(&mut state, old, new, *dtype, true);
                }
            }
        }

        Changeset(state.into_iter().collect())
    }
}

fn collapse_rename(
    state: &mut HashMap<String, Change>,
    old: &str,
    new: &str,
    dtype: DType,
    overwrites: bool,
) {
    // Self-rename is a no-op.
    if old == new {
        return;
    }

    // Detect round-trip: if source was Renamed/Replaced from the
    // destination, the file is returning to its original path — no-op.
    if let Some(Change::Renamed { from: origin, .. } | Change::Replaced { from: origin, .. }) =
        state.get(old)
    {
        if origin == new {
            // Round-trip: a→old→a. Remove intermediate state.
            state.remove(old);
            state.remove(new);
            return;
        }
    }

    // If destination already had a rename, re-insert Deleted for its origin.
    if let Some(
        Change::Renamed {
            from: prior_origin, ..
        }
        | Change::Replaced {
            from: prior_origin, ..
        },
    ) = state.remove(new)
    {
        state.entry(prior_origin).or_insert(Change::Deleted);
    }

    // Handle prior state at source.
    match state.remove(old) {
        Some(Change::Added { ino, dtype }) => {
            // Source was staged — rename of staged is just an Add at new path.
            state.insert(new.to_string(), Change::Added { ino, dtype });
            return;
        }
        Some(Change::Modified { dtype: prev_dt, .. }) => {
            // Source was modified — preserve dtype from source.
            state.insert(old.to_string(), Change::Deleted);
            insert_rename(state, old, new, prev_dt, overwrites);
            return;
        }
        _ => {
            state.insert(old.to_string(), Change::Deleted);
        }
    }

    insert_rename(state, old, new, dtype, overwrites);
}

fn insert_rename(
    state: &mut HashMap<String, Change>,
    old: &str,
    new: &str,
    dtype: DType,
    overwrites: bool,
) {
    if overwrites {
        state.insert(
            new.to_string(),
            Change::Replaced {
                from: old.to_string(),
                dtype,
            },
        );
    } else {
        state.insert(
            new.to_string(),
            Change::Renamed {
                from: old.to_string(),
                dtype,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ensure_parent ────────────────────────────────────────────────

    #[test]
    fn ensure_parent_creates_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("a").join("b").join("file.txt");
        let mut cache = HashSet::new();

        ensure_parent(&file_path, &mut cache).unwrap();

        let parent = file_path.parent().unwrap();
        assert!(parent.exists());
        assert!(cache.contains(parent));
    }

    #[test]
    fn ensure_parent_caches_and_skips_second_call() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("c").join("file.txt");
        let mut cache = HashSet::new();

        ensure_parent(&file_path, &mut cache).unwrap();
        assert!(cache.contains(file_path.parent().unwrap()));

        // Remove the directory so we can detect if it would be recreated
        fs::remove_dir_all(file_path.parent().unwrap()).unwrap();

        // Second call should skip creation because parent is cached
        ensure_parent(&file_path, &mut cache).unwrap();

        // Directory should still be gone — ensure_parent skipped it
        assert!(!file_path.parent().unwrap().exists());
    }

    #[test]
    fn ensure_parent_root_path() {
        let root = Path::new("/");
        let mut cache = HashSet::new();

        // Root has no parent that needs creating; should succeed without error
        ensure_parent(root, &mut cache).unwrap();
        assert!(cache.is_empty());
    }

    // ── collapse ─────────────────────────────────────────────────────

    #[test]
    fn collapse_add() {
        let al = ActionList(vec![Action::Add {
            path: "/a".into(),
            ino: 1,
            dtype: DType::File,
        }]);
        let cs = al.collapse();
        assert_eq!(cs.0.len(), 1);
        assert!(matches!(&cs.0[0], (p, Change::Added { ino: 1, .. }) if p == "/a"));
    }

    #[test]
    fn collapse_modify() {
        let al = ActionList(vec![Action::Modify {
            path: "/a".into(),
            ino: 2,
            dtype: DType::File,
        }]);
        let cs = al.collapse();
        assert_eq!(cs.0.len(), 1);
        assert!(matches!(&cs.0[0], (p, Change::Modified { ino: 2, .. }) if p == "/a"));
    }

    #[test]
    fn collapse_delete() {
        let al = ActionList(vec![Action::Delete { path: "/a".into() }]);
        let cs = al.collapse();
        assert_eq!(cs.0.len(), 1);
        assert!(matches!(&cs.0[0], (p, Change::Deleted) if p == "/a"));
    }

    #[test]
    fn collapse_rename() {
        let al = ActionList(vec![Action::Rename {
            old: "/a".into(),
            new: "/b".into(),
            dtype: DType::File,
        }]);
        let cs = al.collapse();
        assert_eq!(cs.0.len(), 2);
        let has_renamed = cs.0
            .iter()
            .any(|(p, c)| matches!(c, Change::Renamed { from, .. } if from == "/a" && p == "/b"));
        let has_deleted = cs.0
            .iter()
            .any(|(p, c)| matches!(c, Change::Deleted if p == "/a"));
        assert!(has_renamed, "expected Renamed(/a→/b), got: {cs:?}");
        assert!(has_deleted, "expected Deleted(/a), got: {cs:?}");
    }

    #[test]
    fn collapse_replace() {
        let al = ActionList(vec![Action::Replace {
            old: "/a".into(),
            new: "/b".into(),
            dtype: DType::File,
        }]);
        let cs = al.collapse();
        assert_eq!(cs.0.len(), 2);
        let has_replaced = cs.0
            .iter()
            .any(|(p, c)| matches!(c, Change::Replaced { from, .. } if from == "/a" && p == "/b"));
        assert!(has_replaced, "expected Replaced(/a→/b), got: {cs:?}");
    }

    #[test]
    fn collapse_add_then_delete_cancels() {
        let al = ActionList(vec![
            Action::Add {
                path: "/a".into(),
                ino: 1,
                dtype: DType::File,
            },
            Action::Delete { path: "/a".into() },
        ]);
        let cs = al.collapse();
        assert!(cs.0.is_empty(), "A+D should cancel, got: {cs:?}");
    }

    #[test]
    fn collapse_rename_then_delete() {
        let al = ActionList(vec![
            Action::Rename {
                old: "/a".into(),
                new: "/b".into(),
                dtype: DType::File,
            },
            Action::Delete { path: "/b".into() },
        ]);
        let cs = al.collapse();
        // Rename(a→b) then DEL(b) → Deleted(a) + Deleted(b)
        assert_eq!(cs.0.len(), 2, "expected 2 deletes, got: {cs:?}");
        let has_del_a = cs.0
            .iter()
            .any(|(p, c)| matches!(c, Change::Deleted if p == "/a"));
        let has_del_b = cs.0
            .iter()
            .any(|(p, c)| matches!(c, Change::Deleted if p == "/b"));
        assert!(has_del_a, "expected Deleted(/a), got: {cs:?}");
        assert!(has_del_b, "expected Deleted(/b), got: {cs:?}");
    }

    #[test]
    fn collapse_rename_dir_dtype() {
        let al = ActionList(vec![Action::Rename {
            old: "/olddir".into(),
            new: "/newdir".into(),
            dtype: DType::Dir,
        }]);
        let cs = al.collapse();
        let has_renamed = cs.0.iter().any(|(to, c)| {
            matches!(c, Change::Renamed { from, dtype: DType::Dir, .. } if from == "/olddir" && to == "/newdir")
        });
        assert!(
            has_renamed,
            "expected Renamed with DType::Dir, got: {cs:?}"
        );
    }

    #[test]
    fn collapse_rename_then_add_at_old_path() {
        // Rename(a→b) + Add(a, ino=2): old path gets Deleted then overwritten by Add
        let al = ActionList(vec![
            Action::Rename {
                old: "/a".into(),
                new: "/b".into(),
                dtype: DType::File,
            },
            Action::Add {
                path: "/a".into(),
                ino: 2,
                dtype: DType::File,
            },
        ]);
        let cs = al.collapse();
        assert_eq!(cs.0.len(), 2, "expected 2 changes, got: {cs:?}");
        let has_renamed = cs.0.iter().any(|(to, c)| {
            matches!(c, Change::Renamed { from, .. } if from == "/a" && to == "/b")
        });
        let has_added = cs.0
            .iter()
            .any(|(p, c)| matches!(c, Change::Added { ino: 2, .. } if p == "/a"));
        assert!(has_renamed, "expected Renamed(a→b), got: {cs:?}");
        assert!(has_added, "expected Added(a, ino=2), got: {cs:?}");
    }

    #[test]
    fn collapse_three_cycle_renames() {
        // Three uncollapsed renames forming a cycle: a→tmp, b→a, tmp→b
        let al = ActionList(vec![
            Action::Rename {
                old: "/a".into(),
                new: "/tmp".into(),
                dtype: DType::File,
            },
            Action::Rename {
                old: "/b".into(),
                new: "/a".into(),
                dtype: DType::File,
            },
            Action::Rename {
                old: "/tmp".into(),
                new: "/b".into(),
                dtype: DType::File,
            },
        ]);
        let cs = al.collapse();
        assert!(!cs.0.is_empty(), "swap should produce changes, got: {cs:?}");
    }

    #[test]
    fn collapse_delete_then_modify_same_path() {
        // DEL(x) + MOD(x): collapse keeps latest (Modified)
        let al = ActionList(vec![
            Action::Delete { path: "/x".into() },
            Action::Modify {
                path: "/x".into(),
                ino: 2,
                dtype: DType::File,
            },
        ]);
        let cs = al.collapse();
        assert_eq!(cs.0.len(), 1, "expected 1 change, got: {cs:?}");
        let has_modified = cs.0
            .iter()
            .any(|(p, c)| matches!(c, Change::Modified { ino: 2, .. } if p == "/x"));
        assert!(has_modified, "expected Modified, got: {cs:?}");
    }

    #[test]
    fn collapse_replace_overwrite_tracking() {
        // REP(a→b) then REP(c→b): b's prior origin (a) should be deleted
        let al = ActionList(vec![
            Action::Replace {
                old: "/a".into(),
                new: "/b".into(),
                dtype: DType::File,
            },
            Action::Replace {
                old: "/c".into(),
                new: "/b".into(),
                dtype: DType::File,
            },
        ]);
        let cs = al.collapse();
        let has_del_a = cs.0
            .iter()
            .any(|(p, c)| matches!(c, Change::Deleted if p == "/a"));
        let has_del_c = cs.0
            .iter()
            .any(|(p, c)| matches!(c, Change::Deleted if p == "/c"));
        let has_replaced = cs.0
            .iter()
            .any(|(p, c)| matches!(c, Change::Replaced { from, .. } if from == "/c" && p == "/b"));
        assert!(
            has_del_a,
            "expected Deleted(/a) from overwrite, got: {cs:?}"
        );
        assert!(has_del_c, "expected Deleted(/c) as source, got: {cs:?}");
        assert!(has_replaced, "expected Replaced(c→b), got: {cs:?}");
    }

    #[test]
    fn collapse_self_rename_is_noop() {
        let al = ActionList(vec![Action::Rename {
            old: "/a".into(),
            new: "/a".into(),
            dtype: DType::File,
        }]);
        let cs = al.collapse();
        assert!(cs.0.is_empty(), "RDR(a,a) should be a no-op, got: {cs:?}");
    }

    #[test]
    fn collapse_self_replace_is_noop() {
        let al = ActionList(vec![Action::Replace {
            old: "/a".into(),
            new: "/a".into(),
            dtype: DType::File,
        }]);
        let cs = al.collapse();
        assert!(cs.0.is_empty(), "REP(a,a) should be a no-op, got: {cs:?}");
    }

    #[test]
    fn collapse_roundtrip_cancels() {
        // a→tmp→a should cancel both entries.
        let al = ActionList(vec![
            Action::Rename {
                old: "/a".into(),
                new: "/tmp".into(),
                dtype: DType::File,
            },
            Action::Rename {
                old: "/tmp".into(),
                new: "/a".into(),
                dtype: DType::File,
            },
        ]);
        let cs = al.collapse();
        assert!(cs.0.is_empty(), "a→tmp→a should cancel, got: {cs:?}");
    }

    #[test]
    fn collapse_modify_then_rename_preserves_dtype() {
        let al = ActionList(vec![
            Action::Modify {
                path: "/a".into(),
                ino: 1,
                dtype: DType::Dir,
            },
            Action::Rename {
                old: "/a".into(),
                new: "/b".into(),
                dtype: DType::File,
            },
        ]);
        let cs = al.collapse();
        let renamed = cs.0
            .iter()
            .find(|(p, _)| p == "/b")
            .expect("expected entry at /b");
        assert!(
            matches!(&renamed.1, Change::Renamed { dtype: DType::Dir, .. }),
            "dtype should come from Modify (Dir), got: {:?}",
            renamed.1
        );
    }
}
