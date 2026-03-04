//! Change detection and commit functionality.

use crate::sandbox::Sandbox;
use anyhow::Result;
use colored::Colorize;
use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub enum Change {
    Added(PathBuf),
    Modified(PathBuf),
    Deleted(PathBuf),
    CreatedDir(PathBuf),
    Symlink(PathBuf),
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Change::Added(p) => write!(f, "{} {}", p.display(), "(added)".green()),
            Change::Modified(p) => write!(f, "{} {}", p.display(), "(modified)".yellow()),
            Change::Deleted(p) => write!(f, "{} {}", p.display(), "(deleted)".red()),
            Change::CreatedDir(p) => write!(f, "{} {}", p.display(), "(created dir)".cyan()),
            Change::Symlink(p) => write!(f, "{} {}", p.display(), "(symlink)".blue()),
        }
    }
}

pub fn detect_changes(sandbox: &Sandbox) -> Result<Vec<Change>> {
    let mut changes = Vec::new();
    if !sandbox.upperdir.exists() {
        return Ok(changes);
    }

    for entry in WalkDir::new(&sandbox.upperdir).min_depth(1) {
        let entry = entry?;
        let overlay_path = entry.path();
        let relative = overlay_path.strip_prefix(&sandbox.upperdir)?;
        let real_path = Path::new("/").join(relative);

        let metadata = fs::symlink_metadata(overlay_path)?;
        let file_type = metadata.file_type();

        if file_type.is_symlink() {
            changes.push(Change::Symlink(real_path));
        } else if file_type.is_dir() {
            if !real_path.exists() {
                changes.push(Change::CreatedDir(real_path));
            }
        } else if file_type.is_char_device() {
            changes.push(Change::Deleted(real_path));
        } else if file_type.is_file() {
            if real_path.exists() {
                changes.push(Change::Modified(real_path));
            } else {
                changes.push(Change::Added(real_path));
            }
        }
    }
    Ok(changes)
}

pub fn show_summary(sandbox: &Sandbox) -> Result<bool> {
    let changes = detect_changes(sandbox)?;
    if changes.is_empty() {
        println!("\n{}", "No changes detected.".yellow());
        return Ok(false);
    }
    println!(
        "\n{}",
        "Changes detected in the following files:".green().bold()
    );
    println!();
    for change in &changes {
        println!("  {}", change);
    }
    Ok(true)
}

pub fn commit_changes(sandbox: &Sandbox) -> Result<()> {
    let changes = detect_changes(sandbox)?;
    if changes.is_empty() {
        println!("{}", "No changes to commit.".yellow());
        return Ok(());
    }

    for change in changes {
        match change {
            Change::Added(real_path) | Change::Modified(real_path) => {
                let overlay_path = sandbox.upperdir.join(real_path.strip_prefix("/")?);
                if let Some(parent) = real_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                if real_path.exists() {
                    fs::remove_file(&real_path).or_else(|_| fs::remove_dir_all(&real_path))?;
                }
                fs::copy(&overlay_path, &real_path)?;
                println!("  {} {}", "Committed:".green(), real_path.display());
            }
            Change::Deleted(real_path) => {
                if real_path.exists() {
                    fs::remove_file(&real_path).or_else(|_| fs::remove_dir_all(&real_path))?;
                    println!("  {} {}", "Deleted:".red(), real_path.display());
                }
            }
            Change::CreatedDir(real_path) => {
                fs::create_dir_all(&real_path)?;
                println!("  {} {}", "Created dir:".cyan(), real_path.display());
            }
            Change::Symlink(real_path) => {
                let overlay_path = sandbox.upperdir.join(real_path.strip_prefix("/")?);
                let target = fs::read_link(&overlay_path)?;
                if real_path.exists() || real_path.is_symlink() {
                    fs::remove_file(&real_path)?;
                }
                std::os::unix::fs::symlink(&target, &real_path)?;
                println!(
                    "  {} {} -> {}",
                    "Symlink:".blue(),
                    real_path.display(),
                    target.display()
                );
            }
        }
    }
    println!("\n{}", "Changes committed successfully.".green().bold());
    Ok(())
}
