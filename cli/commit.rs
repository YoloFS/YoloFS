// agfs CLI — commit.rs
//
// `agfs commit` — apply staged changes to base.
// Journal is resolved first, then changes are applied sequentially.

use crate::journal;
use crate::resolve::{self, Change};
use crate::utils::to_base_path;
use anyhow::{Context, Result};
use colored::Colorize;
use std::collections::HashSet;
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
    let staged = journal::inode_path(agfs_dir, ino);
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

    // Restore original permissions for modified files.
    if let Some(perms) = original_perms {
        fs::set_permissions(base_path, perms)
            .with_context(|| format!("restoring permissions on {}", base_path.display()))?;
    }

    Ok(())
}

fn apply_changes(agfs: &Path, changes: &[Change]) -> Result<()> {
    let mut ensured: HashSet<PathBuf> = HashSet::new();

    for change in changes {
        match change {
            Change::Renamed { from, to, .. } => {
                let base_old = to_base_path(from);
                let base_new = to_base_path(to);
                ensure_parent(&base_new, &mut ensured)?;
                // Guard: source may not exist in base when a staged-only
                // file was renamed; the content is handled by a separate
                // Modified change via apply_inode.
                if base_old.exists() {
                    fs::rename(&base_old, &base_new)
                        .with_context(|| format!("rename {from} → {to}"))?;
                }
            }
            Change::Deleted(p) => {
                let base_file = to_base_path(p);
                if base_file.exists() {
                    if base_file.is_dir() {
                        fs::remove_dir_all(&base_file)
                    } else {
                        fs::remove_file(&base_file)
                    }
                    .with_context(|| format!("deleting {p}"))?;
                }
            }
            Change::Added { path, ino, .. } | Change::Modified { path, ino, .. } => {
                let base_file = to_base_path(path);
                apply_inode(agfs, *ino, &base_file, &mut ensured)?;
            }
        }
    }

    Ok(())
}

pub fn run() -> Result<()> {
    let agfs = crate::utils::session_dir()?;

    let records = journal::read(&agfs)?.records;
    let live = resolve::extract_live(records);
    let changes = resolve::resolve(live)?;

    if changes.is_empty() {
        println!("{}", "Nothing to commit.".yellow());
        return Ok(());
    }

    apply_changes(&agfs, &changes)?;
    let committed = changes.len();

    crate::abort::reset_staging(&agfs)?;

    println!(
        "{}",
        format!(
            "Committed {committed} change{}.",
            crate::utils::plural(committed)
        )
        .green()
        .bold()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::Path;

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
}
