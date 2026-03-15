use agfs::config::{Config, MountConfig};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use systemd::journal::{self, JournalSeek};

/// Uses the `agfs` binary from PATH (installed via `make install`).
pub const AGFS_BIN: &str = "agfs";

/// Seek the system journal to its current tail and return the cursor of the
/// last entry.  Returns `None` if the journal is empty or cannot be opened.
fn snapshot_journal() -> Option<String> {
    let mut j = journal::OpenOptions::default()
        .system(true)
        .local_only(true)
        .open()
        .map_err(|e| eprintln!("kernel-log check: could not open journal: {e}"))
        .ok()?;
    j.seek(JournalSeek::Tail).ok()?;
    // Move to the last actual entry so we have a valid cursor.
    let entry = j
        .previous_entry()
        .map_err(|e| eprintln!("kernel-log check: journal seek failed: {e}"))
        .ok()??;
    drop(entry);
    j.cursor()
        .map_err(|e| eprintln!("kernel-log check: could not read cursor: {e}"))
        .ok()
}

/// Return all kernel-transport messages that arrived after `cursor`.
fn kernel_messages_since(cursor: &str) -> Vec<String> {
    let mut messages = Vec::new();
    let Ok(mut j) = journal::OpenOptions::default()
        .system(true)
        .local_only(true)
        .open()
        .map_err(|e| eprintln!("kernel-log check: could not open journal: {e}"))
    else {
        return messages;
    };
    if j.seek(JournalSeek::Cursor {
        cursor: cursor.to_string(),
    })
    .is_err()
    {
        return messages;
    }
    // Advance past the cursor entry before applying the filter.  With an
    // active match, next_entry() skips non-matching entries, so if we added
    // the filter first we would silently drop the first real kernel message.
    let _ = j.next_entry();
    // Now restrict iteration to entries from the kernel ring buffer.
    let _ = j.match_add("_TRANSPORT", "kernel");
    while let Ok(Some(record)) = j.next_entry() {
        let msg = record
            .get("MESSAGE")
            .cloned()
            .unwrap_or_else(|| "<no message>".to_string());
        messages.push(msg);
    }
    messages
}

/// A managed agfs session for testing, driven entirely through the CLI.
///
/// Creates a temp directory, seeds base files, and uses `agfs mount` /
/// `agfs commit` / `agfs abort` for the full lifecycle.
///
/// On drop the systemd journal is checked for any kernel messages produced
/// since the session started.  If any are found the test fails with a panic.
pub struct AgfsSession {
    pub root: PathBuf,
    pub mnt: PathBuf,
    mounted: bool,
    journal_cursor: Option<String>,
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
            journal_cursor: None,
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
        // Snapshot the journal before mounting so we can detect any kernel
        // messages (warnings, errors, BUG/WARN traces) produced by this session.
        self.journal_cursor = snapshot_journal();

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
        // Check the journal for unexpected kernel messages before tearing down.
        // Guard against double-panic: skip the check if we are already unwinding.
        let kernel_msgs = if !std::thread::panicking() {
            self.journal_cursor
                .as_deref()
                .map(kernel_messages_since)
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        if self.mounted {
            let _ = Command::new(AGFS_BIN)
                .arg("unmount")
                .current_dir(&self.root)
                .env("NO_COLOR", "1")
                .output();
            self.mounted = false;
        }
        let _ = fs::remove_dir_all(&self.root);

        if !kernel_msgs.is_empty() {
            panic!(
                "Unexpected kernel messages during test:\n  {}",
                kernel_msgs.join("\n  ")
            );
        }
    }
}
