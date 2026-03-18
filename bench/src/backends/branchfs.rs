use crate::backend::{self, Backend};
use crate::workload::{CacheMode, IterResult, Workload};
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

pub struct BranchFs;

impl Backend for BranchFs {
    fn name(&self) -> &'static str {
        "branchfs"
    }

    fn unsupported_reason(&self, workload: &dyn Workload) -> Option<&'static str> {
        let cold_staged_metadata = workload.name().starts_with("meta-")
            && workload.cache_mode() == CacheMode::DropPageCache
            && workload.needs_prepare_workdir();
        if cold_staged_metadata {
            Some(
                "cold metadata on staged/checkpoint files cannot be measured on branchfs: \
                  flushing FUSE daemon state requires unmounting, which loses branch state",
            )
        } else {
            None
        }
    }

    fn available(&self) -> bool {
        which("branchfs")
    }

    fn unavailable_reason(&self) -> Option<&'static str> {
        if !which("branchfs") {
            Some("'branchfs' not found in PATH (install with: make -C bench install)")
        } else {
            None
        }
    }

    fn run_one(&self, workload: &dyn Workload, verbose: bool) -> Result<(IterResult, Vec<String>)> {
        let cache = dirs_next::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("agfs-bench");
        std::fs::create_dir_all(&cache)?;

        let root = tempfile::Builder::new()
            .prefix("agfs-bench-branchfs-")
            .tempdir_in(&cache)
            .context("creating branchfs session tempdir")?;

        let base_dir = root.path().join("base");
        let storage_dir = root.path().join("storage");
        let mnt_dir = root.path().join("mnt");
        std::fs::create_dir_all(&base_dir)?;
        std::fs::create_dir_all(&storage_dir)?;
        std::fs::create_dir_all(&mnt_dir)?;

        // Populate base directory before mounting (not timed).
        let base_work = base_dir.join(workload.work_dir());
        std::fs::create_dir_all(&base_work)?;
        workload.populate_base(&base_work)?;

        let pipe = if verbose {
            Stdio::inherit()
        } else {
            Stdio::null()
        };

        // Init: mount + create branch.
        let t_init = Instant::now();
        let out = Command::new("branchfs")
            .arg("mount")
            .arg("--base")
            .arg(&base_dir)
            .arg("--storage")
            .arg(&storage_dir)
            .arg(&mnt_dir)
            .stdout(pipe)
            .stderr(if verbose {
                Stdio::inherit()
            } else {
                Stdio::piped()
            })
            .output()
            .context("running branchfs mount")?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!("branchfs mount failed: {stderr}");
        }

        // Create a branch for this iteration.
        let out = Command::new("branchfs")
            .arg("create")
            .arg("bench")
            .arg(&mnt_dir)
            .arg("--storage")
            .arg(&storage_dir)
            .stdout(Stdio::null())
            .stderr(if verbose {
                Stdio::inherit()
            } else {
                Stdio::piped()
            })
            .output()
            .context("running branchfs create")?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            unmount(&mnt_dir, &storage_dir);
            bail!("branchfs create failed: {stderr}");
        }

        let init_ms = t_init.elapsed().as_millis() as u64;

        // Run the workload inside the branch via subprocess.
        let dest = mnt_dir.join(workload.work_dir());
        std::fs::create_dir_all(&dest)?;
        if workload.needs_prepare_workdir() {
            workload.prepare_workdir(&dest)?;
        }
        if workload.needs_checkpoint() {
            // Checkpoint: create a nested branch parented on "bench".
            // Files from "bench" are visible in the child but writes
            // trigger branchfs's copy-on-write.
            let out = Command::new("branchfs")
                .arg("create")
                .arg("bench-chkpt")
                .arg(&mnt_dir)
                .arg("--parent")
                .arg("bench")
                .arg("--storage")
                .arg(&storage_dir)
                .stdout(Stdio::null())
                .stderr(if verbose {
                    Stdio::inherit()
                } else {
                    Stdio::piped()
                })
                .output()
                .context("running branchfs create for checkpoint")?;
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                unmount(&mnt_dir, &storage_dir);
                bail!("branchfs create for checkpoint failed: {stderr}");
            }
        }
        std::fs::metadata(&dest)
            .with_context(|| format!("warming branchfs FUSE path for {}", dest.display()))?;
        let cold = workload.cache_mode() == CacheMode::DropPageCache;
        let mut cmd = backend::exec_workload_cmd(workload.name(), &dest, verbose, cold)?;
        cmd.stderr(if verbose {
            Stdio::inherit()
        } else {
            Stdio::piped()
        });
        let sub = match backend::run_workload_subprocess(&mut cmd, cold) {
            Ok(r) => r,
            Err(e) => {
                unmount(&mnt_dir, &storage_dir);
                return Err(e.context("workload failed under branchfs"));
            }
        };
        let staging_ms = sub.staging_ms;

        // Commit the branch back to base.
        let t1 = Instant::now();
        let out = Command::new("branchfs")
            .arg("commit")
            .arg(&mnt_dir)
            .arg("--storage")
            .arg(&storage_dir)
            .stdout(Stdio::null())
            .stderr(if verbose {
                Stdio::inherit()
            } else {
                Stdio::piped()
            })
            .output()
            .context("running branchfs commit")?;
        let commit_ms = t1.elapsed().as_millis() as u64;

        let commit_ok = out.status.success();
        let commit_stderr = String::from_utf8_lossy(&out.stderr).to_string();

        // Always unmount.
        unmount(&mnt_dir, &storage_dir);

        if !commit_ok {
            bail!("branchfs commit failed: {commit_stderr}");
        }

        Ok((
            IterResult {
                init_ms: Some(init_ms),
                staging_ms: Some(staging_ms),
                commit_ms: Some(commit_ms),
                total_ms: init_ms + staging_ms + commit_ms,
                op_result: sub.op_result,
                checkpoint_series: sub.checkpoint_series,
            },
            vec![],
        ))
    }
}

fn unmount(mnt_dir: &std::path::Path, storage_dir: &std::path::Path) {
    let _ = Command::new("branchfs")
        .arg("unmount")
        .arg(mnt_dir)
        .arg("--storage")
        .arg(storage_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();
}

fn which(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
