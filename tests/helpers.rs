use anyhow::{Context, Result, bail};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use yolofs::config::Config;
use yolofs::kmsg;

pub const YOLO_BIN: &str = "yolo";

/// A managed yolofs session for testing, driven entirely through the CLI.
///
/// Creates a temp directory, seeds base files, and uses `yolo mount` /
/// `yolo commit` / `yolo abort` for the full lifecycle.
///
/// On drop the kernel ring buffer is checked for any kernel messages produced
/// since the session started.  If any are found the test fails with a panic.
pub struct YoloSession {
    pub root: PathBuf,
    pub mnt: PathBuf,
    mounted: bool,
    cursor: Option<kmsg::KmsgCursor>,
}

impl YoloSession {
    /// Create a new test session with a custom yolofs.toml config.
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

        config.save(&root.join("yolofs.toml"))?;

        let mnt = root.join(".yolofs/mnt");

        let mut session = Self {
            root,
            mnt,
            mounted: false,
            cursor: None,
        };
        session.mount()?;
        Ok(session)
    }

    /// Wrap an existing mounted session root (for tests that do custom setup).
    pub fn from_existing_root(root: std::path::PathBuf) -> Result<Self> {
        let mnt = root.join(".yolofs/mnt");
        let cursor = Some(kmsg::KmsgCursor::now().context("could not open /dev/kmsg")?);
        Ok(Self {
            root,
            mnt,
            mounted: true,
            cursor,
        })
    }

    /// Create a new test session: seed files, write yolofs.toml, `yolo mount`.
    pub fn new() -> Result<Self> {
        Self::new_with_config(Config {
            permission: false,
            ..Default::default()
        })
    }

    fn mount(&mut self) -> Result<()> {
        // Checkpoint the kernel ring buffer before mounting so we can detect any
        // kernel messages (warnings, errors, BUG/WARN traces) produced by this
        // session.
        self.cursor = Some(
            kmsg::KmsgCursor::now()
                .context("could not open /dev/kmsg — set kernel.dmesg_restrict=0")?,
        );

        let output = Command::new(YOLO_BIN)
            .arg("mount")
            .current_dir(&self.root)
            .env("NO_COLOR", "1")
            .output()
            .context("running yolo mount")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("yolofs mount failed: {stderr}");
        }
        self.mounted = true;
        Ok(())
    }

    /// Resolve a relative path through the yolofs mount.
    /// e.g., "hello.txt" → <mnt>/<root>/hello.txt
    pub fn mnt_path(&self, rel: &str) -> PathBuf {
        self.mnt
            .join(self.root.strip_prefix("/").unwrap_or(&self.root))
            .join(rel)
    }

    /// Resolve a base (host) path.
    pub fn base_path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Get the inode store directory path.
    pub fn inodes_dir(&self) -> PathBuf {
        self.root.join(".yolofs/inodes")
    }

    /// Run an yolo CLI subcommand from the session root, return stdout.
    pub fn cli(&self, args: &[&str]) -> Result<String> {
        let output = Command::new(YOLO_BIN)
            .args(args)
            .current_dir(&self.root)
            .env("NO_COLOR", "1")
            .output()
            .context("running yolo CLI")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            bail!(
                "yolo {:?} failed ({}): stdout={} stderr={}",
                args,
                output.status,
                stdout,
                stderr
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Run an yolo CLI subcommand and return (success, stdout, stderr).
    pub fn cli_output(&self, args: &[&str]) -> Result<(bool, String, String)> {
        let output = Command::new(YOLO_BIN)
            .args(args)
            .current_dir(&self.root)
            .env("NO_COLOR", "1")
            .output()
            .context("running yolo CLI")?;
        Ok((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ))
    }

    /// Run an yolo CLI subcommand and return the raw exit code.
    pub fn cli_exit_code(&self, args: &[&str]) -> Result<i32> {
        let output = Command::new(YOLO_BIN)
            .args(args)
            .current_dir(&self.root)
            .env("NO_COLOR", "1")
            .output()
            .context("running yolo CLI")?;
        Ok(output.status.code().unwrap_or(-1))
    }

    /// Run a command under yolofs via `yolo run -q --` and return exit code.
    pub fn run_in_yolofs(&self, cmd: &[&str]) -> Result<i32> {
        let mut args = vec!["run", "-q", "--"];
        args.extend_from_slice(cmd);
        self.cli_exit_code(&args)
    }
}

impl Drop for YoloSession {
    fn drop(&mut self) {
        // Check the kernel ring buffer for unexpected messages before tearing
        // down.  Guard against double-panic: skip the check if already unwinding.
        let kernel_msgs = if !std::thread::panicking() {
            self.cursor
                .as_ref()
                .map(|c| c.read_new().expect("failed to read /dev/kmsg"))
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        if self.mounted {
            let _ = Command::new(YOLO_BIN)
                .args(["unmount", "--force"])
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
