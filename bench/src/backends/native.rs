use crate::backend::{self, Backend};
use crate::workload::{IterResult, Workload};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Stdio;

pub struct Native;

fn cache_base() -> Result<PathBuf> {
    let base = dirs_next::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("agfs-bench");
    std::fs::create_dir_all(&base).context("creating agfs-bench cache dir")?;
    Ok(base)
}

impl Backend for Native {
    fn name(&self) -> &'static str {
        "native"
    }

    fn run_one(&self, workload: &dyn Workload, verbose: bool) -> Result<(IterResult, Vec<String>)> {
        let root = tempfile::Builder::new()
            .prefix("agfs-bench-")
            .tempdir_in(cache_base()?)
            .context("creating session tempdir")?;

        let dest = root.path().join(workload.work_dir());
        std::fs::create_dir_all(&dest)?;

        let mut cmd = backend::exec_workload_cmd(workload.name(), &dest, verbose)?;
        cmd.stderr(if verbose {
            Stdio::inherit()
        } else {
            Stdio::piped()
        });

        let result = backend::run_workload_subprocess(&mut cmd)?;

        Ok((
            IterResult {
                init_ms: None,
                staging_ms: None,
                commit_ms: None,
                total_ms: result.staging_ms,
            },
            vec![],
        ))
    }
}
