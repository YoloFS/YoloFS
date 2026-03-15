// agfs CLI — commit.rs
//
// `agfs commit` — apply staged changes to base (§3.10).
// `agfs commit --at <name>` — partial commit up to a snapshot (§3.11.4).
// Journal is resolved first, then changes are applied sequentially.

use crate::ioctl;
use crate::journal::{self, Change};
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

/// Apply a staging blob to base. Stats the blob to determine type.
fn apply_blob(
    agfs_dir: &Path,
    blob_id: u64,
    base_path: &Path,
    ensured: &mut HashSet<PathBuf>,
) -> Result<()> {
    let blob = journal::blob_path(agfs_dir, blob_id);
    let meta = fs::symlink_metadata(&blob)
        .with_context(|| format!("stat staging blob {}", blob.display()))?;

    ensure_parent(base_path, ensured)?;

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
        let target = fs::read_link(&blob)?;
        std::os::unix::fs::symlink(&target, base_path)
            .with_context(|| format!("creating symlink at {}", base_path.display()))?;
    } else if meta.is_dir() {
        fs::create_dir_all(base_path).with_context(|| format!("mkdir {}", base_path.display()))?;
    } else {
        fs::rename(&blob, base_path)
            .or_else(|_| {
                fs::copy(&blob, base_path)?;
                fs::remove_file(&blob)?;
                Ok::<_, std::io::Error>(())
            })
            .with_context(|| format!("moving blob to {}", base_path.display()))?;
    }
    Ok(())
}

fn apply_changes(agfs: &Path, changes: &[Change]) -> Result<usize> {
    let mut committed = 0;
    let mut ensured: HashSet<PathBuf> = HashSet::new();

    for change in changes {
        match change {
            Change::Renamed { from, to } => {
                let base_old = to_base_path(from);
                let base_new = to_base_path(to);
                ensure_parent(&base_new, &mut ensured)?;
                fs::rename(&base_old, &base_new)
                    .with_context(|| format!("rename {from} → {to}"))?;
            }
            Change::RenamedModified { from, to, blob_id } => {
                let base_old = to_base_path(from);
                let base_new = to_base_path(to);
                ensure_parent(&base_new, &mut ensured)?;
                if base_old.exists() {
                    fs::rename(&base_old, &base_new)
                        .with_context(|| format!("rename {from} → {to}"))?;
                }
                apply_blob(agfs, *blob_id, &base_new, &mut ensured)?;
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
            Change::Added { path, blob_id } | Change::Modified { path, blob_id } => {
                let base_file = to_base_path(path);
                apply_blob(agfs, *blob_id, &base_file, &mut ensured)?;
            }
        }
        committed += 1;
    }

    Ok(committed)
}

pub fn run(at: Option<&str>) -> Result<()> {
    let agfs = crate::utils::session_dir()?;

    match at {
        Some(name) => run_partial(&agfs, name),
        None => run_full(&agfs),
    }
}

fn run_full(agfs: &Path) -> Result<()> {
    let changes = journal::resolve(agfs)?;

    if changes.is_empty() {
        println!("{}", "Nothing to commit.".yellow());
        return Ok(());
    }

    let committed = apply_changes(agfs, &changes)?;

    crate::abort::reset_staging(agfs)?;

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

fn run_partial(agfs: &Path, snapshot_name: &str) -> Result<()> {
    let (changes, remaining) = journal::split_at_snapshot(agfs, snapshot_name)?;

    if changes.is_empty() {
        println!("{}", "Nothing to commit at this snapshot.".yellow());
        return Ok(());
    }

    let committed = apply_changes(agfs, &changes)?;

    // Clean up staging blobs that weren't moved by apply_blob (dirs, symlinks)
    for change in &changes {
        if let Some(id) = change.blob_id() {
            let blob = journal::blob_path(agfs, id);
            if blob.exists() {
                let _ = fs::remove_file(&blob);
            }
        }
    }

    // Rewrite journal: keep only records after the snapshot
    let journal_path = agfs.join("journal");
    let tmp_path = agfs.join("journal.tmp");
    journal::write_records(&tmp_path, &remaining)?;
    fs::rename(&tmp_path, &journal_path).context("replacing journal")?;

    // Signal kernel to invalidate caches (reopens journal fd)
    let ctl_file = ioctl::open(agfs).context("opening ctl for cache invalidation")?;
    ioctl::invalidate_cache(&ctl_file).context("invalidating cache")?;

    println!(
        "{}",
        format!(
            "Committed {committed} change{} (up to snapshot \"{snapshot_name}\").",
            crate::utils::plural(committed)
        )
        .green()
        .bold()
    );

    Ok(())
}
