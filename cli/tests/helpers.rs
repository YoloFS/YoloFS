use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A managed agfs session for testing, driven entirely through the CLI.
///
/// Creates a temp directory, seeds base files, and uses `agfs mount` /
/// `agfs commit` / `agfs abort` for the full lifecycle.
pub struct AgfsSession {
    pub root: PathBuf,
    pub mnt: PathBuf,
    mounted: bool,
}

/// Locate the agfs CLI binary (release build).
fn agfs_bin() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.join("target/release/agfs")
}

impl AgfsSession {
    /// Create a new test session: seed files, write agfs.toml, `agfs mount`.
    pub fn new() -> Result<Self> {
        let root = tempfile::tempdir()
            .context("creating temp dir")?
            .keep();

        // Seed base test files
        fs::write(root.join("hello.txt"), "base content\n")?;
        fs::write(root.join("multi.txt"), "line1\nline2\n")?;
        fs::create_dir_all(root.join("subdir"))?;
        fs::write(root.join("subdir/deep.txt"), "nested\n")?;

        // Write agfs.toml with noperm for testing
        fs::write(
            root.join("agfs.toml"),
            "[mount]\nnoperm = true\n\n[rules]\n",
        )?;

        let mnt = root.join(".agfs/mnt");

        let mut session = Self {
            root,
            mnt,
            mounted: false,
        };
        session.mount()?;
        Ok(session)
    }

    fn mount(&mut self) -> Result<()> {
        let output = Command::new(agfs_bin())
            .arg("mount")
            .current_dir(&self.root)
            .env("NO_COLOR", "1")
            .output()
            .context("running agfs mount")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("agfs mount failed: {stderr}");
        }
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
        self.root
            .join(".agfs/staging")
            .join(self.root.strip_prefix("/").unwrap())
            .join(rel)
    }

    /// Run an agfs CLI subcommand from the session root, return stdout.
    pub fn cli(&self, args: &[&str]) -> Result<String> {
        let output = Command::new(agfs_bin())
            .args(args)
            .current_dir(&self.root)
            .env("NO_COLOR", "1")
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

    /// Run an agfs CLI subcommand and return (success, stdout, stderr).
    pub fn cli_output(&self, args: &[&str]) -> Result<(bool, String, String)> {
        let output = Command::new(agfs_bin())
            .args(args)
            .current_dir(&self.root)
            .env("NO_COLOR", "1")
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
            // Use CLI abort to cleanly unmount + remove .agfs/
            let _ = Command::new(agfs_bin())
                .arg("abort")
                .current_dir(&self.root)
                .env("NO_COLOR", "1")
                .output();
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
        let mods = std::fs::read_to_string("/proc/modules").unwrap_or_default();
        if !mods.contains("agfs ") {
            eprintln!("SKIPPED: agfs module not loaded");
            return;
        }
    };
}
