// agfs CLI — commit.rs
//
// `agfs commit` — apply staged changes to base (§3.6).

use crate::{ctl, status, unmount};
use anyhow::{Context, Result};
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

pub fn run() -> Result<()> {
    let agfs = ctl::agfs_dir()?;
    if !agfs.exists() {
        anyhow::bail!("no agfs session found (no .agfs/ directory)");
    }

    let staging_dir = agfs.join("staging");
    let base = Path::new("/");

    let changes = status::staging_walk(&agfs)?;

    if changes.is_empty() {
        println!("nothing to commit");
        return Ok(());
    }

    let mut committed = 0;

    for change in &changes {
        match change {
            status::Change::Renamed { from, to } => {
                // Pure rename: rename(base/old, base/new)
                let base_old = base.join(from.trim_start_matches('/'));
                let base_new = base.join(to.trim_start_matches('/'));
                ensure_parent(&base_new)?;
                fs::rename(&base_old, &base_new)
                    .with_context(|| format!("rename {from} → {to}"))?;
                committed += 1;
            }
            status::Change::RenamedModified { from, to } => {
                // Staged file at new_path: rename staging→base, then unlink old
                let staging_file = staging_dir.join(to.trim_start_matches('/'));
                let base_new = base.join(to.trim_start_matches('/'));
                let base_old = base.join(from.trim_start_matches('/'));
                ensure_parent(&base_new)?;
                fs::rename(&staging_file, &base_new)
                    .with_context(|| format!("commit renamed+modified {to}"))?;
                let _ = if base_old.is_dir() {
                    fs::remove_dir_all(&base_old)
                } else {
                    fs::remove_file(&base_old)
                }; // best effort
                committed += 1;
            }
            status::Change::Deleted(p) => {
                // Whiteout → delete base file
                let base_file = base.join(p.trim_start_matches('/'));
                if base_file.is_dir() {
                    let _ = fs::remove_dir_all(&base_file);
                } else {
                    let _ = fs::remove_file(&base_file);
                }
                // Remove the whiteout from staging
                let staging_wh = staging_dir.join(p.trim_start_matches('/'));
                let _ = fs::remove_file(&staging_wh);
                committed += 1;
            }
            status::Change::Added(p) | status::Change::Modified(p) => {
                // rename staging/<path> → base/<path>
                let staging_file = staging_dir.join(p.trim_start_matches('/'));
                let base_file = base.join(p.trim_start_matches('/'));
                ensure_parent(&base_file)?;
                fs::rename(&staging_file, &base_file)
                    .with_context(|| format!("commit {p}"))?;
                committed += 1;
            }
        }
    }

    // Clean up staging directory and renames file
    if staging_dir.exists() {
        let _ = fs::remove_dir_all(&staging_dir);
        fs::create_dir_all(&staging_dir)
            .context("recreating staging dir")?;
    }
    let renames_path = agfs.join("renames");
    if renames_path.exists() {
        let _ = fs::remove_file(&renames_path);
    }

    // Signal kernel to invalidate caches
    if let Ok(ctl_file) = ctl::open_ctl(&agfs) {
        let _ = ctl::ioctl_invalidate_cache(&ctl_file);
    }

    println!("committed {committed} change{}", if committed == 1 { "" } else { "s" });

    // Unmount after commit
    unmount::run()?;

    Ok(())
}
