// yolo CLI — commit.rs
//
// `yolo commit` — apply staged changes to base.
//
// Builds a DirTree from live journal segments, converts it to a commit
// plan (see journal/plan.rs), then applies each action in execution order.

use crate::journal::Journal;
use anyhow::{Context, Result};
use colored::Colorize;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

// ── Apply helpers ─────────────────────────────────────────────────────

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

/// Apply a staged inode to base.
fn apply_stage(
    yolo_dir: &Path,
    ino: u32,
    base_path: &Path,
    ensured: &mut HashSet<PathBuf>,
) -> Result<()> {
    let staged = crate::utils::inode_path(yolo_dir, ino);
    let meta = fs::symlink_metadata(&staged)
        .with_context(|| format!("stat staged inode {}", staged.display()))?;

    ensure_parent(base_path, ensured)?;

    // Remove whatever exists at the target, unless we're staging a dir
    // over an existing dir (children may have been placed by renames).
    if let Ok(existing) = base_path.symlink_metadata() {
        let both_dirs = meta.is_dir() && existing.is_dir() && !existing.file_type().is_symlink();
        if !both_dirs {
            remove_existing(base_path, &existing)?;
        }
    }

    let is_symlink = meta.file_type().is_symlink();
    if is_symlink {
        let target = fs::read_link(&staged)?;
        std::os::unix::fs::symlink(&target, base_path)
            .with_context(|| format!("creating symlink at {}", base_path.display()))?;
    } else if meta.is_dir() {
        fs::create_dir_all(base_path).with_context(|| format!("mkdir {}", base_path.display()))?;
        fs::set_permissions(base_path, meta.permissions())
            .with_context(|| format!("setting dir permissions on {}", base_path.display()))?;
    } else {
        fs::rename(&staged, base_path)
            .or_else(|_| {
                fs::copy(&staged, base_path)?;
                fs::remove_file(&staged)?;
                fs::set_permissions(base_path, meta.permissions())?;
                Ok::<_, std::io::Error>(())
            })
            .with_context(|| format!("moving inode to {}", base_path.display()))?;
    }

    Ok(())
}

/// Rename a base path, removing anything at the destination first.
fn apply_rename(src: &Path, dst: &Path, ensured: &mut HashSet<PathBuf>) -> Result<()> {
    ensure_parent(dst, ensured)?;
    if let Ok(existing) = dst.symlink_metadata() {
        remove_existing(dst, &existing)?;
    }
    fs::rename(src, dst).with_context(|| format!("renaming {} → {}", src.display(), dst.display()))
}

fn apply_delete(path: &Path) -> Result<()> {
    if let Ok(meta) = path.symlink_metadata() {
        remove_existing(path, &meta)?;
    }
    Ok(())
}

// ── Apply ─────────────────────────────────────────────────────────────

fn apply_plan(yolofs: &Path, plan: &crate::journal::CommitPlan) -> Result<usize> {
    use crate::journal::types::Action;
    let mut ensured: HashSet<PathBuf> = HashSet::new();

    for action in plan.iter() {
        match action {
            Action::Rename { src, dst } => {
                apply_rename(
                    &crate::utils::to_base_path(src),
                    &crate::utils::to_base_path(dst),
                    &mut ensured,
                )?;
            }
            Action::Delete { path } => {
                apply_delete(&crate::utils::to_base_path(path))?;
            }
            Action::Stage { path, ino } => {
                apply_stage(
                    yolofs,
                    *ino,
                    &crate::utils::to_base_path(path),
                    &mut ensured,
                )?;
            }
        }
    }

    Ok(plan.len())
}

// ── Entry point ───────────────────────────────────────────────────────

pub fn run() -> Result<()> {
    let yolofs = crate::utils::session_dir()?;

    let journal = Journal::read(&yolofs)?;
    let plan = journal.into_tree().into_plan();

    if plan.is_empty() {
        println!("{}", "Nothing to commit.".yellow());
        return Ok(());
    }

    let committed = apply_plan(&yolofs, &plan)?;

    super::abort::reset_staging(&yolofs)?;

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

        fs::remove_dir_all(file_path.parent().unwrap()).unwrap();

        ensure_parent(&file_path, &mut cache).unwrap();

        assert!(!file_path.parent().unwrap().exists());
    }

    #[test]
    fn ensure_parent_root_path() {
        let root = Path::new("/");
        let mut cache = HashSet::new();

        ensure_parent(root, &mut cache).unwrap();
        assert!(cache.is_empty());
    }
}
