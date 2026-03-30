use crate::backend::{self, Backend, CheckpointController, CheckpointOutcome};
use crate::workload::{CacheMode, IterResult, Workload, WorkloadKind};
use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

pub struct BranchFs;

fn branchfs_workload_recreates_workdir(workload: &dyn Workload) -> bool {
    matches!(workload.name(), "dev-workflow" | "mini-dev-workflow")
}

struct BranchFsCheckpointController {
    mnt_dir: PathBuf,
    storage_dir: PathBuf,
    resume_dir: PathBuf,
    current_branch: String,
    /// Number of nested checkpoint branches created (for commit unwinding).
    depth: usize,
}

impl CheckpointController for BranchFsCheckpointController {
    fn checkpoint(&mut self, step: usize) -> Result<CheckpointOutcome> {
        let t = Instant::now();
        let next = format!("bench-step-{step:03}");
        let out = Command::new("branchfs")
            .arg("create")
            .arg(&next)
            .arg(&self.mnt_dir)
            .arg("--parent")
            .arg(&self.current_branch)
            .arg("--storage")
            .arg(&self.storage_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .context("running branchfs create for checkpoint")?;
        if !out.status.success() {
            bail!(
                "branchfs checkpoint create failed at step {step}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        self.current_branch = next;
        self.depth += 1;
        // branchfs auto-switches the mount to the new branch. Resume from the
        // backend-selected stable cwd for this workload.
        Ok(CheckpointOutcome::Continue {
            checkpoint_ms: t.elapsed().as_millis() as u64,
            next_dest: Some(self.resume_dir.clone()),
        })
    }
}

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

    fn default_skip_reason(&self, workload: &dyn Workload) -> Option<&'static str> {
        if workload.name() == "dev-workflow" {
            Some("dev-workflow skips branchfs by default; use --backend branchfs to include")
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
        let recreates_workdir = branchfs_workload_recreates_workdir(workload);
        if !recreates_workdir {
            std::fs::create_dir_all(&base_work)?;
            workload.populate_base(&base_work)?;
        }

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
        if !recreates_workdir {
            std::fs::create_dir_all(&dest)?;
        }
        if workload.needs_prepare_workdir() {
            workload.prepare_workdir(&dest)?;
        }
        let eager_checkpoint_branch = workload.needs_checkpoint() && !recreates_workdir;
        if eager_checkpoint_branch {
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
        let warm_path = if recreates_workdir { &mnt_dir } else { &dest };
        std::fs::metadata(warm_path)
            .with_context(|| format!("warming branchfs FUSE path for {}", warm_path.display()))?;
        let cold = workload.cache_mode() == CacheMode::DropPageCache;
        let mut cmd = if workload.kind() == WorkloadKind::Macro {
            let mut c = backend::exec_workload_cmd(workload.name(), &dest, verbose, cold)?;
            c.current_dir(if recreates_workdir { &mnt_dir } else { &dest });
            c
        } else {
            let mut c = backend::exec_workload_cmd(
                workload.name(),
                std::path::Path::new("."),
                verbose,
                cold,
            )?;
            c.current_dir(&dest);
            c
        };
        cmd.stderr(if verbose {
            Stdio::inherit()
        } else {
            Stdio::piped()
        });
        let sub = match backend::spawn_and_await_ready(&mut cmd, cold) {
            Ok(sp) => {
                if cold {
                    crate::workloads::drop_page_cache()
                        .context("dropping page cache after subprocess READY")?;
                }
                sp
            }
            Err(e) => {
                unmount(&mnt_dir, &storage_dir);
                return Err(e.context("workload failed under branchfs"));
            }
        };
        let mut cp = BranchFsCheckpointController {
            mnt_dir: mnt_dir.clone(),
            storage_dir: storage_dir.clone(),
            resume_dir: if recreates_workdir {
                mnt_dir.clone()
            } else {
                mnt_dir.join(workload.work_dir())
            },
            current_branch: if eager_checkpoint_branch {
                "bench-chkpt".to_string()
            } else {
                "bench".to_string()
            },
            depth: 0,
        };
        let sub = match sub.go_with_checkpoint(&mut cp) {
            Ok(r) => r,
            Err(e) => {
                unmount(&mnt_dir, &storage_dir);
                return Err(e.context("workload failed under branchfs"));
            }
        };
        let staging_ms = sub.staging_ms;

        // Commit all branch levels back to base. The checkpoint controller
        // may have created nested branches (bench-step-001, bench-step-002, ...);
        // branchfs commit only merges one level at a time, so we repeat.
        // Levels: checkpoint depth + 1 for "bench" (+ 1 for eager "bench-chkpt" if present).
        let commit_levels = cp.depth
            + 1 // the initial "bench" branch
            + if eager_checkpoint_branch { 1 } else { 0 };
        let t1 = Instant::now();
        for _ in 0..commit_levels {
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
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                unmount(&mnt_dir, &storage_dir);
                bail!("branchfs commit failed: {stderr}");
            }
        }
        let commit_ms = t1.elapsed().as_millis() as u64;

        // Always unmount.
        unmount(&mnt_dir, &storage_dir);

        Ok((
            IterResult {
                init_ms: Some(init_ms),
                staging_ms: Some(staging_ms),
                status_us: None, // branchfs has no status mechanism
                commit_ms: Some(commit_ms),
                total_ms: init_ms + staging_ms + commit_ms,
                op_result: sub.op_result,
                checkpoint_series: sub.checkpoint_series,
                macro_step_series: sub.macro_step_series,
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
