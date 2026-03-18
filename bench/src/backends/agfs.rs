use crate::backend::{self, Backend};
use crate::workload::{CacheMode, IterResult, Workload};
use agfs::config::{Config, Perm};
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

// ── Session (mount / unmount lifecycle) ──────────────────────────────────────

struct Session {
    root: tempfile::TempDir,
}

impl Session {
    /// Set up the session: write config, populate base, `agfs mount`.
    /// Returns (session, init_ms) where init_ms is the wall time of mount.
    fn setup(config: Config, workload: &dyn Workload) -> Result<(Self, u64)> {
        let cache = dirs_next::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("agfs-bench");
        std::fs::create_dir_all(&cache).context("creating agfs-bench cache dir")?;

        let root = tempfile::Builder::new()
            .prefix("agfs-bench-")
            .tempdir_in(&cache)
            .context("creating session tempdir")?;

        config
            .save(&root.path().join("agfs.toml"))
            .context("writing agfs.toml")?;

        // Populate base directory before mounting (not timed).
        let base_work = root.path().join(workload.work_dir());
        std::fs::create_dir_all(&base_work)?;
        workload.populate_base(&base_work)?;

        let t = Instant::now();
        let out = Command::new("agfs")
            .arg("mount")
            .current_dir(root.path())
            .env("NO_COLOR", "1")
            .output()
            .context("running agfs mount")?;
        let init_ms = t.elapsed().as_millis() as u64;

        if !out.status.success() {
            bail!(
                "agfs mount failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        Ok((Session { root }, init_ms))
    }

    fn mnt_path(&self, rel: &str) -> PathBuf {
        let root = self.root.path();
        root.join(".agfs/mnt")
            .join(root.strip_prefix("/").unwrap_or(root))
            .join(rel)
    }

    fn checkpoint(&self) -> Result<()> {
        let out = Command::new("agfs")
            .arg("checkpoint")
            .arg("bench-checkpoint")
            .current_dir(self.root.path())
            .env("NO_COLOR", "1")
            .output()
            .context("running agfs checkpoint")?;
        if !out.status.success() {
            bail!(
                "agfs checkpoint failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(())
    }

    fn commit(&self, verbose: bool) -> Result<u64> {
        let t = Instant::now();
        let out = Command::new("agfs")
            .arg("commit")
            .current_dir(self.root.path())
            .env("NO_COLOR", "1")
            .stdout(if verbose {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .output()
            .context("running agfs commit")?;
        let ms = t.elapsed().as_millis() as u64;
        if !out.status.success() {
            bail!(
                "agfs commit failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(ms)
    }

    fn journal_debug(&self) -> String {
        let agfs_dir = self.root.path().join(".agfs");
        match agfs::journal::read(&agfs_dir) {
            Ok(journal) => journal
                .records
                .iter()
                .map(|r| format!("  {r:?}"))
                .collect::<Vec<_>>()
                .join("\n"),
            Err(_) => String::new(),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = Command::new("agfs")
            .arg("unmount")
            .current_dir(self.root.path())
            .env("NO_COLOR", "1")
            .output();
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn run_agfs_iteration(
    config: Config,
    workload: &dyn Workload,
    verbose: bool,
) -> Result<(IterResult, Vec<String>)> {
    let (session, init_ms) = Session::setup(config, workload)?;

    let dest = session.mnt_path(workload.work_dir());
    // For cold+base workloads, skip create_dir_all on the mounted path —
    // the directory already exists in the lower fs via populate_base().
    // Traversing the agfs mount here would warm kernel state that
    // drop_caches cannot flush (igrab'd directory inodes).
    let cold = workload.cache_mode() == CacheMode::DropPageCache;
    if !cold || workload.needs_prepare_workdir() {
        std::fs::create_dir_all(&dest)?;
    }
    if workload.needs_prepare_workdir() {
        workload.prepare_workdir(&dest)?;
    }
    if workload.needs_checkpoint() {
        session.checkpoint()?;
    }
    // For cold workloads, page caches are dropped after READY (inside
    // run_workload_subprocess). The mount-point dentry chain stays pinned
    // in the VFS — that's inherent to any mounted filesystem and means
    // cold stat through agfs will always be faster than cold stat on native
    // (fewer unpinned path components to read from disk).
    let cold = workload.cache_mode() == CacheMode::DropPageCache;
    let mut cmd = backend::exec_workload_cmd(workload.name(), &dest, verbose, cold)?;
    cmd.stderr(if verbose {
        Stdio::inherit()
    } else {
        Stdio::piped()
    });
    let result = match backend::run_workload_subprocess(&mut cmd, cold) {
        Ok(r) => r,
        Err(e) => {
            let journal = session.journal_debug();
            if !journal.is_empty() {
                eprintln!("    agfs journal at failure:\n{journal}");
            }
            return Err(e);
        }
    };

    let commit_ms = match session.commit(verbose) {
        Ok(ms) => ms,
        Err(e) => {
            let journal = session.journal_debug();
            if !journal.is_empty() {
                eprintln!("    agfs journal at failure:\n{journal}");
            }
            return Err(e);
        }
    };

    Ok((
        IterResult {
            init_ms: Some(init_ms),
            staging_ms: Some(result.staging_ms),
            commit_ms: Some(commit_ms),
            total_ms: init_ms + result.staging_ms + commit_ms,
            op_result: result.op_result,
        },
        vec![],
    ))
}

// ── agfs-allow-all ───────────────────────────────────────────────────────────

pub struct AgfsAllowAll;

fn cold_staged_reason(workload: &dyn Workload) -> Option<&'static str> {
    let cold_staged_metadata = workload.name().starts_with("meta-")
        && workload.cache_mode() == CacheMode::DropPageCache
        && workload.needs_prepare_workdir();
    if cold_staged_metadata {
        Some(
            "cold metadata on staged/checkpoint files cannot be measured on agfs: \
              flushing the kernel dirent table requires unmounting, which loses staging state",
        )
    } else {
        None
    }
}

impl Backend for AgfsAllowAll {
    fn name(&self) -> &'static str {
        "agfs-allow-all"
    }

    fn unsupported_reason(&self, workload: &dyn Workload) -> Option<&'static str> {
        cold_staged_reason(workload)
    }

    fn run_one(&self, workload: &dyn Workload, verbose: bool) -> Result<(IterResult, Vec<String>)> {
        let config = Config {
            permission: false,
            checkpoint: false,
            rules: BTreeMap::from([("/".to_string(), Perm::AllowRw)]),
            ..Default::default()
        };
        run_agfs_iteration(config, workload, verbose)
    }
}

// ── agfs-realistic ───────────────────────────────────────────────────────────

pub struct AgfsRealistic;

impl Backend for AgfsRealistic {
    fn name(&self) -> &'static str {
        "agfs-realistic"
    }

    fn unsupported_reason(&self, workload: &dyn Workload) -> Option<&'static str> {
        cold_staged_reason(workload)
    }

    fn run_one(&self, workload: &dyn Workload, verbose: bool) -> Result<(IterResult, Vec<String>)> {
        let cache = dirs_next::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("agfs-bench");
        std::fs::create_dir_all(&cache)?;
        let root = tempfile::Builder::new()
            .prefix("agfs-bench-")
            .tempdir_in(&cache)?;

        let mut rules = BTreeMap::new();
        for (path, perm) in workload.realistic_rules(root.path()) {
            rules.insert(path, perm);
        }
        let config = Config {
            permission: false,
            checkpoint: false,
            rules,
            ..Default::default()
        };
        config.save(&root.path().join("agfs.toml"))?;

        // Populate base directory before mounting (not timed).
        let base_work = root.path().join(workload.work_dir());
        std::fs::create_dir_all(&base_work)?;
        workload.populate_base(&base_work)?;

        let t_init = Instant::now();
        let out = Command::new("agfs")
            .arg("mount")
            .current_dir(root.path())
            .env("NO_COLOR", "1")
            .output()
            .context("running agfs mount")?;
        let init_ms = t_init.elapsed().as_millis() as u64;

        if !out.status.success() {
            bail!(
                "agfs mount failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        let session = Session { root };
        let dest = session.mnt_path(workload.work_dir());
        let cold = workload.cache_mode() == CacheMode::DropPageCache;
        if !cold || workload.needs_prepare_workdir() {
            std::fs::create_dir_all(&dest)?;
        }
        if workload.needs_prepare_workdir() {
            workload.prepare_workdir(&dest)?;
        }
        if workload.needs_checkpoint() {
            session.checkpoint()?;
        }
        let mut cmd = backend::exec_workload_cmd(workload.name(), &dest, verbose, cold)?;
        cmd.stderr(if verbose {
            Stdio::inherit()
        } else {
            Stdio::piped()
        });
        let result = match backend::run_workload_subprocess(&mut cmd, cold) {
            Ok(r) => r,
            Err(e) => {
                let journal = session.journal_debug();
                if !journal.is_empty() {
                    eprintln!("    agfs journal at failure:\n{journal}");
                }
                return Err(e);
            }
        };

        let commit_ms = match session.commit(verbose) {
            Ok(ms) => ms,
            Err(e) => {
                let journal = session.journal_debug();
                if !journal.is_empty() {
                    eprintln!("    agfs journal at failure:\n{journal}");
                }
                return Err(e);
            }
        };

        Ok((
            IterResult {
                init_ms: Some(init_ms),
                staging_ms: Some(result.staging_ms),
                commit_ms: Some(commit_ms),
                total_ms: init_ms + result.staging_ms + commit_ms,
                op_result: result.op_result,
            },
            vec![],
        ))
    }
}

// ── For profiler (needs Session access) ──────────────────────────────────────

/// Set up an agfs session for profiling. Returns the session and the workload
/// destination path.
pub fn setup_profile_session(
    workload: &dyn Workload,
    allow_all: bool,
) -> Result<(ProfileSession, PathBuf)> {
    let cache = dirs_next::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("agfs-bench");
    std::fs::create_dir_all(&cache)?;
    let root = tempfile::Builder::new()
        .prefix("agfs-bench-")
        .tempdir_in(&cache)?;

    let config = if allow_all {
        Config {
            permission: false,
            rules: BTreeMap::from([("/".to_string(), Perm::AllowRw)]),
            ..Default::default()
        }
    } else {
        let mut rules = BTreeMap::new();
        for (path, perm) in workload.realistic_rules(root.path()) {
            rules.insert(path, perm);
        }
        Config {
            permission: false,
            rules,
            ..Default::default()
        }
    };

    config.save(&root.path().join("agfs.toml"))?;

    // Populate base directory before mounting.
    let base_work = root.path().join(workload.work_dir());
    std::fs::create_dir_all(&base_work)?;
    workload.populate_base(&base_work)?;

    let out = Command::new("agfs")
        .arg("mount")
        .current_dir(root.path())
        .env("NO_COLOR", "1")
        .output()
        .context("running agfs mount")?;
    if !out.status.success() {
        bail!(
            "agfs mount failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let mnt_path = {
        let r = root.path();
        r.join(".agfs/mnt")
            .join(r.strip_prefix("/").unwrap_or(r))
            .join(workload.work_dir())
    };

    Ok((
        ProfileSession {
            _session: Session { root },
        },
        mnt_path,
    ))
}

/// Opaque handle that keeps the agfs mount alive for profiling.
pub struct ProfileSession {
    _session: Session,
}

impl ProfileSession {
    pub fn commit(&self, verbose: bool) -> Result<()> {
        self._session.commit(verbose)?;
        Ok(())
    }
}
