// agfs CLI — commit.rs
//
// `agfs commit` — apply staged changes to base (§3.10).
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
        fs::create_dir_all(base_path)
            .with_context(|| format!("mkdir {}", base_path.display()))?;
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

pub fn run() -> Result<()> {
    let agfs = crate::session_dir()?;

    let staging_dir = agfs.join("staging");
    let base = Path::new("/");

    let changes = journal::resolve(&agfs)?;

    if changes.is_empty() {
        println!("{}", "Nothing to commit.".yellow());
        return Ok(());
    }

    let mut committed = 0;

    // Journal is resolved and sorted: adds/modifies (parents before children),
    // then deletes (children before parents).
    for change in &changes {
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
                apply_blob(&agfs, *blob_id, &base_new)?;
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
            Change::Added { path, blob_id }
            | Change::Modified { path, blob_id } => {
                let base_file = base.join(path.trim_start_matches('/'));
                apply_blob(&agfs, *blob_id, &base_file)?;
            }
        }
        committed += 1;
    }

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
    let ctl_file = ioctl::open(&agfs).context("opening ctl for cache invalidation")?;
    ioctl::invalidate_cache(&ctl_file).context("invalidating cache")?;

    println!(
        "{}",
        format!(
            "Committed {committed} change{}.",
            if committed == 1 { "" } else { "s" }
        )
        .green()
        .bold()
    );

    Ok(())
}
