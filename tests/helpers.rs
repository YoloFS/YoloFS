use agfs::config::{Config, MountConfig};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

/// Uses the `agfs` binary from PATH (installed via `make install`).
pub const AGFS_BIN: &str = "agfs";

/// A managed agfs session for testing, driven entirely through the CLI.
///
/// Creates a temp directory, seeds base files, and uses `agfs mount` /
/// `agfs commit` / `agfs abort` for the full lifecycle.
pub struct AgfsSession {
    pub root: PathBuf,
    pub mnt: PathBuf,
    mounted: bool,
}

impl AgfsSession {
    /// Create a new test session with a custom agfs.toml config.
    pub fn new_with_config(config: Config) -> Result<Self> {
        let root = tempfile::tempdir().context("creating temp dir")?.keep();

        // Seed base test files
        fs::write(root.join("hello.txt"), "base content\n")?;
        fs::write(root.join("multi.txt"), "line1\nline2\n")?;
        fs::create_dir_all(root.join("subdir"))?;
        fs::write(root.join("subdir/deep.txt"), "nested\n")?;

        // Seed an executable script for permission tests
        fs::write(root.join("test.sh"), "#!/bin/sh\necho ok\n")?;
        fs::set_permissions(root.join("test.sh"), fs::Permissions::from_mode(0o755))?;

        config.save(&root.join("agfs.toml"))?;

        let mnt = root.join(".agfs/mnt");

        let mut session = Self {
            root,
            mnt,
            mounted: false,
        };
        session.mount()?;
        Ok(session)
    }

    /// Create a new test session: seed files, write agfs.toml, `agfs mount`.
    pub fn new() -> Result<Self> {
        Self::new_with_config(Config {
            mount: MountConfig {
                noperm: true,
                ..Default::default()
            },
            rules: BTreeMap::new(),
        })
    }

    fn mount(&mut self) -> Result<()> {
        let output = Command::new(AGFS_BIN)
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
        self.mnt
            .join(self.root.strip_prefix("/").unwrap())
            .join(rel)
    }

    /// Resolve a base (host) path.
    pub fn base_path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Get the staging directory path.
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join(".agfs/staging")
    }

    /// Get the journal file path.
    pub fn journal_path(&self) -> PathBuf {
        self.root.join(".agfs/journal")
    }

    /// Run an agfs CLI subcommand from the session root, return stdout.
    pub fn cli(&self, args: &[&str]) -> Result<String> {
        let output = Command::new(AGFS_BIN)
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
        let output = Command::new(AGFS_BIN)
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

    /// Run an agfs CLI subcommand and return the raw exit code.
    pub fn cli_exit_code(&self, args: &[&str]) -> Result<i32> {
        let output = Command::new(AGFS_BIN)
            .args(args)
            .current_dir(&self.root)
            .env("NO_COLOR", "1")
            .output()
            .context("running agfs CLI")?;
        Ok(output.status.code().unwrap_or(-1))
    }

    /// Run a command inside the sandbox via `agfs exec --` and return exit code.
    pub fn run_in_sandbox(&self, cmd: &[&str]) -> Result<i32> {
        let mut args = vec!["exec", "--"];
        args.extend_from_slice(cmd);
        self.cli_exit_code(&args)
    }
}

impl Drop for AgfsSession {
    fn drop(&mut self) {
        if self.mounted {
            let _ = Command::new(AGFS_BIN)
                .arg("unmount")
                .current_dir(&self.root)
                .env("NO_COLOR", "1")
                .output();
            self.mounted = false;
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}
