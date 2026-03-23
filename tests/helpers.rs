use agfs::config::Config;
use agfs::kmsg;
use anyhow::{Context, Result, bail};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

pub const AGFS_BIN: &str = "agfs";

/// A managed agfs session for testing, driven entirely through the CLI.
///
/// Creates a temp directory, seeds base files, and uses `agfs mount` /
/// `agfs commit` / `agfs abort` for the full lifecycle.
///
/// On drop the kernel ring buffer is checked for any kernel messages produced
/// since the session started.  If any are found the test fails with a panic.
pub struct AgfsSession {
    pub root: PathBuf,
    pub mnt: PathBuf,
    mounted: bool,
    cursor: Option<kmsg::KmsgCursor>,
    daemon_pid: Option<u32>,
    in_child_process: bool,
}

impl AgfsSession {
    /// Create a new test session with a custom agfs.toml config.
    ///
    /// Returns `Some(session)` in the child process (inside the daemon's
    /// namespace) and `None` in the parent (test-harness thread). Callers
    /// should use `let Some(s) = … else { return };` so the parent
    /// short-circuits while the child runs the test body.
    pub fn new_with_config(config: Config) -> Result<Option<Self>> {
        let session = Self::new_internal(config)?;
        session.fork_into_namespace()
    }

    /// Create a new test session: seed files, write agfs.toml, `agfs mount`.
    pub fn new() -> Result<Option<Self>> {
        Self::new_with_config(Config {
            permission: false,
            ..Default::default()
        })
    }

    /// Set up the temp directory, seed files, mount the daemon. Does NOT
    /// fork — the caller is still in the host namespace.
    fn new_internal(config: Config) -> Result<Self> {
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
            cursor: None,
            daemon_pid: None,
            in_child_process: false,
        };
        session.mount()?;
        Ok(session)
    }

    fn mount(&mut self) -> Result<()> {
        // Checkpoint the kernel ring buffer before mounting so we can detect any
        // kernel messages (warnings, errors, BUG/WARN traces) produced by this
        // session.
        self.cursor = Some(
            kmsg::KmsgCursor::now()
                .context("could not open /dev/kmsg — set kernel.dmesg_restrict=0")?,
        );

        let output = Command::new(AGFS_BIN)
            .arg("mount")
            .current_dir(&self.root)
            .env("NO_COLOR", "1")
            .output()
            .context("running agfs mount")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("agfs mount failed: {stderr}");
        }
        self.mounted = true;

        // Read daemon pid for run_in_namespace().
        let pid_str =
            std::fs::read_to_string(self.root.join(".agfs/pid")).context("reading .agfs/pid")?;
        self.daemon_pid = Some(pid_str.trim().parse().context("parsing daemon pid")?);

        Ok(())
    }

    /// Re-read the daemon PID from .agfs/pid. Call this after unmount+remount
    /// to update the stored PID for run_in_namespace().
    pub fn refresh_daemon_pid(&mut self) -> Result<()> {
        let pid_str = std::fs::read_to_string(self.root.join(".agfs/pid"))
            .context("reading .agfs/pid after remount")?;
        self.daemon_pid = Some(pid_str.trim().parse().context("parsing daemon pid")?);
        Ok(())
    }

    /// Fork and enter the daemon's user + mount namespace.
    ///
    /// - **Child**: setns into the daemon, returns `Ok(Some(self))`.
    /// - **Parent**: waits for the child to exit, returns `Ok(None)`.
    ///   The parent's copy of `self` is dropped normally (unmount, cleanup).
    fn fork_into_namespace(mut self) -> Result<Option<Self>> {
        let pid = self.daemon_pid.expect("daemon not running");

        match unsafe { libc::fork() } {
            -1 => panic!("fork failed: {}", std::io::Error::last_os_error()),
            0 => {
                // Child: setns into daemon's namespace, then return.
                unsafe {
                    let user_ns = std::ffi::CString::new(format!("/proc/{pid}/ns/user")).unwrap();
                    let fd = libc::open(user_ns.as_ptr(), libc::O_RDONLY);
                    if fd < 0 || libc::setns(fd, libc::CLONE_NEWUSER) != 0 {
                        libc::_exit(99);
                    }
                    libc::close(fd);

                    let mnt_ns = std::ffi::CString::new(format!("/proc/{pid}/ns/mnt")).unwrap();
                    let fd = libc::open(mnt_ns.as_ptr(), libc::O_RDONLY);
                    if fd < 0 || libc::setns(fd, libc::CLONE_NEWNS) != 0 {
                        libc::_exit(98);
                    }
                    libc::close(fd);
                }

                self.in_child_process = true;
                Ok(Some(self))
            }
            child_pid => {
                // Parent: wait for child with timeout, then return None.
                // `self` is dropped here → normal cleanup (kmsg, unmount, rm).
                use std::time::{Duration, Instant};
                let deadline = Instant::now() + Duration::from_secs(30);
                loop {
                    let mut status: i32 = 0;
                    let r = unsafe { libc::waitpid(child_pid, &mut status, libc::WNOHANG) };
                    if r > 0 {
                        if libc::WIFEXITED(status) {
                            let code = libc::WEXITSTATUS(status);
                            match code {
                                0 => return Ok(None),
                                99 => panic!("fork_into_namespace: setns(user) failed"),
                                98 => panic!("fork_into_namespace: setns(mnt) failed"),
                                _ => panic!(
                                    "fork_into_namespace: child failed (exit {code})"
                                ),
                            }
                        } else {
                            panic!("fork_into_namespace: child killed by signal");
                        }
                    }
                    if Instant::now() > deadline {
                        unsafe { libc::kill(child_pid, libc::SIGKILL) };
                        panic!("fork_into_namespace: child timed out after 30s");
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    /// Resolve a relative path through the agfs mount.
    /// e.g., "hello.txt" → <mnt>/<root>/hello.txt
    pub fn mnt_path(&self, rel: &str) -> PathBuf {
        self.mnt
            .join(self.root.strip_prefix("/").unwrap_or(&self.root))
            .join(rel)
    }

    /// Run a closure inside the daemon's namespace. Forks a child that
    /// does setns(user) + setns(mnt), runs the closure, and exits.
    /// The parent waits and panics if the child failed.
    ///
    /// Use this only for multi-phase tests that unmount/remount and need
    /// to re-enter a new daemon's namespace. Normal tests get namespace
    /// entry automatically via `new()`.
    pub fn run_in_namespace<F: FnOnce()>(&self, f: F) {
        let pid = self.daemon_pid.expect("daemon not running");

        match unsafe { libc::fork() } {
            -1 => panic!("fork failed: {}", std::io::Error::last_os_error()),
            0 => {
                // Child: setns into daemon's namespace, run closure, exit.
                unsafe {
                    let user_ns = std::ffi::CString::new(format!("/proc/{pid}/ns/user")).unwrap();
                    let fd = libc::open(user_ns.as_ptr(), libc::O_RDONLY);
                    if fd < 0 || libc::setns(fd, libc::CLONE_NEWUSER) != 0 {
                        libc::_exit(99);
                    }
                    libc::close(fd);

                    let mnt_ns = std::ffi::CString::new(format!("/proc/{pid}/ns/mnt")).unwrap();
                    let fd = libc::open(mnt_ns.as_ptr(), libc::O_RDONLY);
                    if fd < 0 || libc::setns(fd, libc::CLONE_NEWNS) != 0 {
                        libc::_exit(98);
                    }
                    libc::close(fd);
                }

                // Catch panics so we can report failure via exit code.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
                unsafe {
                    libc::_exit(if result.is_ok() { 0 } else { 1 });
                }
            }
            child_pid => {
                // Parent: wait for child with timeout.
                use std::time::{Duration, Instant};
                let deadline = Instant::now() + Duration::from_secs(30);
                loop {
                    let mut status: i32 = 0;
                    let r = unsafe { libc::waitpid(child_pid, &mut status, libc::WNOHANG) };
                    if r > 0 {
                        if libc::WIFEXITED(status) {
                            let code = libc::WEXITSTATUS(status);
                            match code {
                                0 => return,
                                99 => panic!("run_in_namespace: setns(user) failed"),
                                98 => panic!("run_in_namespace: setns(mnt) failed"),
                                _ => panic!("run_in_namespace: child panicked (exit {code})"),
                            }
                        } else {
                            panic!("run_in_namespace: child killed by signal");
                        }
                    }
                    if Instant::now() > deadline {
                        unsafe { libc::kill(child_pid, libc::SIGKILL) };
                        panic!("run_in_namespace: child timed out after 30s");
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    /// Resolve a base (host) path.
    pub fn base_path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Get the inode store directory path.
    pub fn inodes_dir(&self) -> PathBuf {
        self.root.join(".agfs/inodes")
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
            bail!(
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
        // Child process: exit immediately — the parent handles cleanup.
        if self.in_child_process {
            let code = if std::thread::panicking() { 1 } else { 0 };
            unsafe { libc::_exit(code) };
        }

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
            let _ = Command::new(AGFS_BIN)
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
