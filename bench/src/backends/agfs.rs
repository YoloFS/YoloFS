use crate::backend::{self, Backend};
use crate::workload::{IterResult, Workload};
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
    /// Set up the session: write config + `agfs mount`.
    /// Returns (session, init_ms) where init_ms is the wall time of mount.
    fn setup(config: Config) -> Result<(Self, u64)> {
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
            Ok(records) => records
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
    let (session, init_ms) = Session::setup(config)?;

    let dest = session.mnt_path(workload.work_dir());

    let mut cmd = backend::exec_workload_cmd(workload.name(), &dest, verbose)?;
    cmd.stderr(if verbose {
        Stdio::inherit()
    } else {
        Stdio::piped()
    });
    let result = match backend::run_workload_subprocess(&mut cmd) {
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
        },
        vec![],
    ))
}

// ── agfs-allow-all ───────────────────────────────────────────────────────────

pub struct AgfsAllowAll;

impl Backend for AgfsAllowAll {
    fn name(&self) -> &'static str {
        "agfs-allow-all"
    }

    fn run_one(&self, workload: &dyn Workload, verbose: bool) -> Result<(IterResult, Vec<String>)> {
        let config = Config {
            permission: false,
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
            rules,
            ..Default::default()
        };
        config.save(&root.path().join("agfs.toml"))?;

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

        let mut cmd = backend::exec_workload_cmd(workload.name(), &dest, verbose)?;
        cmd.stderr(if verbose {
            Stdio::inherit()
        } else {
            Stdio::piped()
        });
        let result = match backend::run_workload_subprocess(&mut cmd) {
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
