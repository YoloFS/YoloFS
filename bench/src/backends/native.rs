use crate::backend::{self, Backend};
use crate::workload::{CacheMode, IterResult, Workload, WorkloadKind};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Stdio;

pub struct Native;

fn cache_base() -> Result<PathBuf> {
    let base = dirs_next::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("yolofs-bench");
    std::fs::create_dir_all(&base).context("creating yolofs-bench cache dir")?;
    Ok(base)
}

impl Backend for Native {
    fn name(&self) -> &'static str {
        "native"
    }

    fn run_one(&self, workload: &dyn Workload, verbose: bool) -> Result<(IterResult, Vec<String>)> {
        let root = tempfile::Builder::new()
            .prefix("yolofs-bench-")
            .tempdir_in(cache_base()?)
            .context("creating session tempdir")?;

        let dest = root.path().join(workload.work_dir());
        std::fs::create_dir_all(&dest)?;
        workload.populate_base(&dest)?;
        if workload.needs_prepare_workdir() {
            workload.prepare_workdir(&dest)?;
        }
        let cold = workload.cache_mode() == CacheMode::DropPageCache;
        let mut cmd = if workload.kind() == WorkloadKind::Macro {
            let mut c = backend::exec_workload_cmd(workload.name(), &dest, verbose, cold)?;
            c.current_dir(root.path());
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

        let result = backend::run_workload_subprocess(&mut cmd, cold)?;

        Ok((
            IterResult {
                init_ms: None,
                staging_ms: None,
                status_us: None,
                commit_ms: None,
                total_ms: result.staging_ms,
                op_result: result.op_result,
                checkpoint_series: result.checkpoint_series,
                macro_step_series: result.macro_step_series,
            },
            vec![],
        ))
    }
}
