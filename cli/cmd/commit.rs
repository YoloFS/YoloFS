// agfs CLI — commit.rs
//
// `agfs commit` — apply staged changes to base.
// Journal records are replayed sequentially on the base filesystem.

use crate::journal::{self, Journal};
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

/// Remove a file, symlink, or directory.
fn remove_existing(path: &Path, meta: &fs::Metadata) -> Result<()> {
    if meta.is_dir() && !meta.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
    .with_context(|| format!("removing {}", path.display()))
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
    let existing_meta = base_path.symlink_metadata().ok();
    let original_perms = existing_meta
        .as_ref()
        .filter(|m| m.is_file())
        .map(|m| m.permissions());

    // Remove whatever exists at the target path
    if let Some(existing) = &existing_meta {
        remove_existing(base_path, existing)?;
    }

    let is_symlink = meta.file_type().is_symlink();
    if is_symlink {
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
        && !is_symlink
    {
        fs::set_permissions(base_path, perms)
            .with_context(|| format!("restoring permissions on {}", base_path.display()))?;
    }

    Ok(())
}

/// Replay live actions sequentially on the base filesystem.
fn apply_records(agfs: &Path, segments: &[journal::Segment]) -> Result<()> {
    let mut ensured: HashSet<PathBuf> = HashSet::new();

    for action in segments.iter().flat_map(|s| &s.records) {
        match action {
            journal::Action::Add { path, ino, .. }
            | journal::Action::Modify { path, ino, .. } => {
                let base_path = crate::utils::to_base_path(path);
                apply_inode(agfs, *ino, &base_path, &mut ensured)?;
            }
            journal::Action::Delete { path, .. } => {
                let base_path = crate::utils::to_base_path(path);
                if let Ok(meta) = base_path.symlink_metadata() {
                    remove_existing(&base_path, &meta)?;
                }
            }
            journal::Action::Rename { dst, src, .. }
            | journal::Action::Replace { dst, src, .. } => {
                let base_src = crate::utils::to_base_path(src);
                let base_dst = crate::utils::to_base_path(dst);
                ensure_parent(&base_dst, &mut ensured)?;
                fs::rename(&base_src, &base_dst)
                    .with_context(|| format!("renaming {} → {}", base_src.display(), base_dst.display()))?;
            }
        }
    }
    Ok(())
}

pub fn run() -> Result<()> {
    let agfs = crate::utils::session_dir()?;

    let journal = Journal::read(&agfs)?;
    let live: Vec<_> = journal.into_live_segments_range(0, usize::MAX).collect();
    let committed: usize = live.iter().map(|s| s.records.len()).sum();

    if committed == 0 {
        println!("{}", "Nothing to commit.".yellow());
        return Ok(());
    }

    apply_records(&agfs, &live)?;

    super::abort::reset_staging(&agfs)?;

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
