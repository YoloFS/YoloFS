//! Sandbox directory management.

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use walkdir::WalkDir;

/// Represents a sandbox directory structure.
pub struct Sandbox {
    pub root: PathBuf,
    pub upperdir: PathBuf,
    pub workdir: PathBuf,
    pub temproot: PathBuf,
}

impl Sandbox {
    /// Create a new sandbox in a temporary directory.
    pub fn new_temp() -> Result<Self> {
        let tempdir = tempfile::Builder::new()
            .prefix("agfs-")
            .tempdir()
            .context("Failed to create temp directory")?;
        let root = tempdir.path().to_path_buf();
        std::mem::forget(tempdir);
        Self::init(root)
    }

    /// Create a sandbox in an existing directory.
    pub fn new_at(path: PathBuf) -> Result<Self> {
        if !path.exists() {
            fs::create_dir_all(&path).context("Failed to create sandbox directory")?;
        }
        Self::init(path)
    }

    fn init(root: PathBuf) -> Result<Self> {
        let upperdir = root.join("upperdir");
        let workdir = root.join("workdir");
        let temproot = root.join("temproot");

        fs::create_dir_all(&upperdir).context("Failed to create upperdir")?;
        fs::create_dir_all(&workdir).context("Failed to create workdir")?;
        fs::create_dir_all(&temproot).context("Failed to create temproot")?;

        Ok(Self {
            root,
            upperdir,
            workdir,
            temproot,
        })
    }

    /// Check if sandbox is valid.
    pub fn is_valid(&self) -> bool {
        if !self.upperdir.exists() && !self.workdir.exists() && !self.temproot.exists() {
            return true;
        }
        if self.temproot.exists() {
            for entry in WalkDir::new(&self.temproot).min_depth(1) {
                if let Ok(e) = entry {
                    if e.file_type().is_file() {
                        return false;
                    }
                }
            }
        }
        true
    }
}
