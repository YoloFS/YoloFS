use anyhow::{Context, Result};
use nix::mount::{mount, umount2, MntFlags, MsFlags};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A managed agfs session for testing. Unmounts + cleans up on drop.
pub struct AgfsSession {
    pub root: PathBuf,
    pub agfs_dir: PathBuf,
    pub mnt: PathBuf,
    pub staging: PathBuf,
    mounted: bool,
}

/// Locate the agfs CLI binary (release build).
fn agfs_bin() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.join("target/release/agfs")
}

impl AgfsSession {
    /// Create a new test session in the given directory.
    /// Seeds some base files and mounts agfs with noperm.
    pub fn new() -> Result<Self> {
        let root = tempfile::tempdir()
            .context("creating temp dir")?
            .keep();
        let agfs_dir = root.join(".agfs");
        let mnt = agfs_dir.join("mnt");
        let staging = agfs_dir.join("staging");

        fs::create_dir_all(&staging).context("creating staging dir")?;
        fs::create_dir_all(&mnt).context("creating mnt dir")?;

        // Seed base test files
        fs::write(root.join("hello.txt"), "base content\n")?;
        fs::write(root.join("multi.txt"), "line1\nline2\n")?;
        fs::create_dir_all(root.join("subdir"))?;
        fs::write(root.join("subdir/deep.txt"), "nested\n")?;

        let mut session = Self {
            root,
            agfs_dir,
            mnt,
            staging,
            mounted: false,
        };
        session.mount()?;
        Ok(session)
    }

    fn mount(&mut self) -> Result<()> {
        let data = format!("noperm,storage={}", self.agfs_dir.display());
        mount(
            Some("none"),
            &self.mnt,
            Some("agfs"),
            MsFlags::empty(),
            Some(data.as_str()),
        )
        .context("mounting agfs")?;
        self.mounted = true;
        Ok(())
    }

    /// Resolve a relative path through the agfs mount.
    /// e.g., "hello.txt" → <mnt>/<root>/hello.txt
    pub fn mnt_path(&self, rel: &str) -> PathBuf {
        self.mnt.join(self.root.strip_prefix("/").unwrap()).join(rel)
    }

    /// Resolve a base (host) path.
    pub fn base_path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Resolve a staging path.
    pub fn staging_path(&self, rel: &str) -> PathBuf {
        self.staging
            .join(self.root.strip_prefix("/").unwrap())
            .join(rel)
    }

    /// Run an agfs CLI subcommand and return stdout.
    pub fn cli(&self, args: &[&str]) -> Result<String> {
        let output = Command::new(agfs_bin())
            .args(args)
            .env("AGFS_SESSION", &self.agfs_dir)
            .output()
            .context("running agfs CLI")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            anyhow::bail!(
                "agfs {:?} failed ({}): stdout={} stderr={}",
                args,
                output.status,
                stdout,
                stderr
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Run an agfs CLI subcommand and return stdout even on failure.
    pub fn cli_output(&self, args: &[&str]) -> Result<(bool, String, String)> {
        let output = Command::new(agfs_bin())
            .args(args)
            .env("AGFS_SESSION", &self.agfs_dir)
            .output()
            .context("running agfs CLI")?;
        Ok((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ))
    }
}

impl Drop for AgfsSession {
    fn drop(&mut self) {
        if self.mounted {
            let _ = umount2(&self.mnt, MntFlags::MNT_DETACH);
            self.mounted = false;
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Skip test if not running as root or module not loaded.
#[macro_export]
macro_rules! skip_if_not_root {
    () => {
        if nix::unistd::geteuid().as_raw() != 0 {
            eprintln!("SKIPPED: must run as root");
            return;
        }
        // Check module is loaded
        let mods = std::fs::read_to_string("/proc/modules").unwrap_or_default();
        if !mods.contains("agfs ") {
            eprintln!("SKIPPED: agfs module not loaded");
            return;
        }
    };
}
