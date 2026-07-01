use anyhow::{Context, Result, bail};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
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

/// Create a session temp dir under $HOME, for any test that builds a yolofs
/// session (seeded base files + `.yolofs` storage).
///
/// yolofs overlays `/` and does not cross submounts, so a session under a
/// submounted tmpfs (the usual `/tmp`) is invisible through the mount and base
/// files read back as ENOENT. $HOME is normally on the root mount; every test
/// must create its session dir through this rather than `tempfile::tempdir()`.
pub fn session_tempdir() -> Result<tempfile::TempDir> {
    let home = PathBuf::from(std::env::var_os("HOME").context("$HOME is not set")?);

    let root_dev = fs::metadata("/").context("stat /")?.dev();
    let home_dev = fs::metadata(&home)
        .with_context(|| format!("stat {}", home.display()))?
        .dev();
    if home_dev != root_dev {
        bail!(
            "$HOME ({}) is not on the root filesystem — yolofs overlays `/` and \
             can't see submounts, so its session files would be invisible",
            home.display()
        );
    }

    tempfile::Builder::new()
        .prefix(".yolofs-test-")
        .tempdir_in(&home)
        .with_context(|| format!("creating session dir in {}", home.display()))
}

impl YoloSession {
    /// Create a new test session with a custom yolofs.toml config.
    pub fn new_with_config(config: Config) -> Result<Self> {
        let root = session_tempdir().context("creating temp dir")?.keep();

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

    /// Run an yolo CLI subcommand from the session root and require success,
    /// returning the raw output for the stream-specific accessors below.
    fn cli_checked(&self, args: &[&str]) -> Result<std::process::Output> {
        let output = Command::new(YOLO_BIN)
            .args(args)
            .current_dir(&self.root)
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
        Ok(output)
    }

    /// Run an yolo CLI subcommand from the session root, return stdout.
    pub fn cli(&self, args: &[&str]) -> Result<String> {
        let output = self.cli_checked(args)?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Run an yolo CLI subcommand from the session root, require success, and
    /// return **stderr** — the status stream ("Output and status reporting" in
    /// docs/cli.md): `committed …`, `staging discarded`, `nothing to commit`, …
    pub fn cli_stderr(&self, args: &[&str]) -> Result<String> {
        let output = self.cli_checked(args)?;
        Ok(String::from_utf8_lossy(&output.stderr).to_string())
    }

    /// Run an yolo CLI subcommand and return (success, stdout, stderr).
    pub fn cli_output(&self, args: &[&str]) -> Result<(bool, String, String)> {
        let output = Command::new(YOLO_BIN)
            .args(args)
            .current_dir(&self.root)
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
            .output()
            .context("running yolo CLI")?;
        Ok(output.status.code().unwrap_or(-1))
    }

    /// Run a command under yolofs via `yolo run --no-review --` and return exit code.
    pub fn run_in_yolofs(&self, cmd: &[&str]) -> Result<i32> {
        let mut args = vec!["run", "--no-review", "--"];
        args.extend_from_slice(cmd);
        self.cli_exit_code(&args)
    }
}

/// A spawned `yolo watch` daemon that has already claimed the ask slot.
///
/// Waits for the daemon's readiness line before returning (instead of a fixed
/// sleep that races daemon startup under load) and drains stderr on a background
/// thread so callers can `kill_and_collect()` everything it logged.
pub struct Watch {
    child: Child,
    stderr: Arc<Mutex<String>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Watch {
    /// Spawn `yolo watch <extra_args>` from @root and block until it prints its
    /// readiness line. stdin is piped (write decisions via `stdin_write`).
    pub fn spawn(root: &Path, extra_args: &[&str]) -> Self {
        let mut child = Command::new(YOLO_BIN)
            .arg("watch")
            .args(extra_args)
            .current_dir(root)
            .env("NO_COLOR", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawning yolo watch");

        let err = child.stderr.take().expect("watch stderr piped");
        let stderr = Arc::new(Mutex::new(String::new()));
        let (ready_tx, ready_rx) = mpsc::channel();
        let buf = Arc::clone(&stderr);
        let reader = std::thread::spawn(move || {
            let mut r = BufReader::new(err);
            let mut line = String::new();
            let mut ready = Some(ready_tx);
            loop {
                line.clear();
                match r.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        buf.lock().unwrap().push_str(&line);
                        if line.contains("watching for permission requests") {
                            if let Some(tx) = ready.take() {
                                let _ = tx.send(());
                            }
                        }
                    }
                }
            }
        });

        ready_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("yolo watch never became ready");
        Watch {
            child,
            stderr,
            reader: Some(reader),
        }
    }

    /// Pre-fill the daemon's stdin with an interactive decision (e.g. b"d\n").
    pub fn stdin_write(&mut self, bytes: &[u8]) {
        self.child
            .stdin
            .as_mut()
            .expect("watch stdin piped")
            .write_all(bytes)
            .expect("writing to watch stdin");
    }

    /// Block (bounded) until the daemon has written @needle to stderr.
    ///
    /// The daemon logs its decision ("→ allow"/"→ deny") *after* put_decision
    /// unblocks the requester, so a caller that reads through the mount and then
    /// immediately kills the daemon can race that log line. Wait for it first.
    pub fn wait_for(&self, needle: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if self.stderr.lock().unwrap().contains(needle) {
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "watch never logged {needle:?}; stderr so far:\n{}",
                    self.stderr.lock().unwrap()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Kill the daemon and return everything it wrote to stderr.
    pub fn kill_and_collect(mut self) -> String {
        self.child.kill().ok();
        let _ = self.child.wait();
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
        std::mem::take(&mut *self.stderr.lock().unwrap())
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
                .args(["unmount"])
                .current_dir(&self.root)
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
