use crate::backend::{self, Backend};
use crate::workload::{IterResult, Workload};
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub struct Try;

impl Backend for Try {
    fn name(&self) -> &'static str {
        "try"
    }

    fn available(&self) -> bool {
        which("try") && try_smoke_test()
    }

    fn unavailable_reason(&self) -> Option<&'static str> {
        if !which("try") {
            Some("'try' not found in PATH (install with: make -C bench install)")
        } else if !try_smoke_test() {
            Some("'try -n -- /bin/true' failed (stale mounts under /tmp? overlayfs issue?)")
        } else {
            None
        }
    }

    fn hidden(&self) -> bool {
        true
    }

    fn run_one(&self, workload: &dyn Workload, verbose: bool) -> Result<(IterResult, Vec<String>)> {
        let cache = dirs_next::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("agfs-bench");
        std::fs::create_dir_all(&cache)?;

        let root = tempfile::Builder::new()
            .prefix("agfs-bench-try-")
            .tempdir_in(&cache)
            .context("creating try session tempdir")?;

        let sandbox_dir = root.path().join("sandbox");
        std::fs::create_dir_all(&sandbox_dir)?;
        let dest = root.path().join(workload.work_dir());
        std::fs::create_dir_all(&dest)?;
        workload.populate_base(&dest)?;

        // Build the inner exec-workload command, then wrap it in try.
        let inner = backend::exec_workload_cmd(workload.name(), &dest, verbose)?;
        let inner_exe = inner.get_program().to_owned();
        let inner_args: Vec<_> = inner.get_args().map(|a| a.to_owned()).collect();

        let mut cmd = Command::new("try");
        cmd.current_dir("/tmp")
            .arg("-n")
            .arg("-D")
            .arg(&sandbox_dir)
            .arg("--")
            .arg(&inner_exe);
        for arg in &inner_args {
            cmd.arg(arg);
        }
        cmd.stderr(if verbose {
            Stdio::inherit()
        } else {
            Stdio::piped()
        });

        let result = backend::run_workload_subprocess(&mut cmd)?;

        // Commit: try commit <sandbox>
        let t1 = std::time::Instant::now();
        let out = Command::new("try")
            .arg("commit")
            .arg(&sandbox_dir)
            .stdout(Stdio::null())
            .stderr(if verbose {
                Stdio::inherit()
            } else {
                Stdio::piped()
            })
            .output()
            .context("running try commit")?;
        let commit_ms = t1.elapsed().as_millis() as u64;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!("try commit failed: {stderr}");
        }

        let total_ms = result.startup_ms + result.staging_ms + commit_ms;

        Ok((
            IterResult {
                init_ms: Some(result.startup_ms),
                staging_ms: Some(result.staging_ms),
                commit_ms: Some(commit_ms),
                total_ms,
            },
            vec![],
        ))
    }
}

fn which(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Probe whether `try` actually works by running a trivial command.
/// Can fail if there are stale submounts under /tmp (blocking overlayfs),
/// or if the kernel doesn't support unprivileged overlayfs.
fn try_smoke_test() -> bool {
    Command::new("try")
        .current_dir("/tmp")
        .args(["-n", "--", "/bin/true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
