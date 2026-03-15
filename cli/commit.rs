// agfs CLI — commit.rs
//
// `agfs commit` — apply staged changes to base (§3.10).
// `agfs commit --at <name>` — partial commit up to a snapshot (§3.11.4).
// Journal is resolved first, then changes are applied sequentially.

use crate::ioctl;
use crate::journal::{self, Change};
use anyhow::{Context, Result};
use colored::Colorize;
use std::fs;
use std::path::Path;

/// Create parent directories for a path.
fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating parent dirs for {}", path.display()))?;
    }
    Ok(())
}

/// Apply a staging blob to base. Stats the blob to determine type.
fn apply_blob(agfs_dir: &Path, blob_id: u64, base_path: &Path) -> Result<()> {
    let blob = journal::blob_path(agfs_dir, blob_id);
    let meta = fs::symlink_metadata(&blob)
        .with_context(|| format!("stat staging blob {}", blob.display()))?;

    ensure_parent(base_path)?;

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
    let base = Path::new("/");
    let mut committed = 0;

    for change in changes {
        match change {
            Change::Renamed { from, to } => {
                let base_old = base.join(from.trim_start_matches('/'));
                let base_new = base.join(to.trim_start_matches('/'));
                ensure_parent(&base_new)?;
                fs::rename(&base_old, &base_new)
                    .with_context(|| format!("rename {from} → {to}"))?;
            }
            Change::RenamedModified { from, to, blob_id } => {
                let base_old = base.join(from.trim_start_matches('/'));
                let base_new = base.join(to.trim_start_matches('/'));
                ensure_parent(&base_new)?;
                if base_old.exists() {
                    fs::rename(&base_old, &base_new)
                        .with_context(|| format!("rename {from} → {to}"))?;
                }
                apply_blob(agfs, *blob_id, &base_new)?;
            }
            Change::Deleted(p) => {
                let base_file = base.join(p.trim_start_matches('/'));
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
                let base_file = base.join(path.trim_start_matches('/'));
                apply_blob(agfs, *blob_id, &base_file)?;
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
    let staging_dir = agfs.join("staging");
    let changes = journal::resolve(agfs)?;

    if changes.is_empty() {
        println!("{}", "Nothing to commit.".yellow());
        return Ok(());
    }

    let committed = apply_changes(agfs, &changes)?;

    // Clean up staging directory and journal file
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir).context("removing staging dir")?;
        fs::create_dir_all(&staging_dir).context("recreating staging dir")?;
    }
    let journal_path = agfs.join("journal");
    if journal_path.exists() {
        fs::remove_file(&journal_path).context("removing journal file")?;
    }

    // Signal kernel to invalidate caches
    let ctl_file = ioctl::open(agfs).context("opening ctl for cache invalidation")?;
    ioctl::invalidate_cache(&ctl_file).context("invalidating cache")?;

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
