use crate::workload::{Workload, WorkloadKind};
use crate::workloads;
use anyhow::Context;
use anyhow::Result;
use std::fs::File;
use std::path::Path;
use std::time::Instant;

pub struct MetaCreate {
    pub count: usize,
}

fn run_create(dest: &Path, count: usize) -> Result<()> {
    std::fs::create_dir_all(dest).context("creating work dir")?;
    let mut latencies = Vec::with_capacity(count);
    let total = Instant::now();

    for i in 0..count {
        let t0 = Instant::now();
        File::create(dest.join(format!("f-{i:05}.dat")))
            .with_context(|| format!("creating f-{i:05}.dat"))?;
        latencies.push(t0.elapsed());
    }

    workloads::emit_op_result(&workloads::summarize_latencies(
        latencies,
        total.elapsed(),
        None,
    ))
}

crate::workloads::define_rust_execution!(
    fn run_meta_create(dest: &Path) -> Result<()> {
        std::fs::create_dir_all(dest).context("creating work dir")?;
        let mut latencies = Vec::with_capacity(workloads::OP_FILE_COUNT);
        let total = Instant::now();

        for i in 0..workloads::OP_FILE_COUNT {
            let t0 = Instant::now();
            File::create(dest.join(format!("f-{i:05}.dat")))
                .with_context(|| format!("creating f-{i:05}.dat"))?;
            latencies.push(t0.elapsed());
        }

        workloads::emit_op_result(&workloads::summarize_latencies(latencies, total.elapsed(), None))
    } => meta_create_execution
);

pub fn details() -> workloads::WorkloadDetails {
    workloads::workload_details(
        "Op benchmark for file creation throughput and latency.",
        "No pre-existing files required. The work directory is created on demand inside the mounted session.",
        None,
        &meta_create_execution(),
        file!(),
    )
}

impl Workload for MetaCreate {
    fn name(&self) -> &'static str {
        match self.count {
            10 => "meta-create-10",
            _ => "meta-create",
        }
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }

    fn description(&self) -> &'static str {
        match self.count {
            10 => "Create 10 empty files (isolates per-create overhead)",
            _ => "Create 10,000 empty files (file creation throughput)",
        }
    }

    fn work_dir(&self) -> &'static str {
        match self.count {
            10 => "meta-create-10",
            _ => "meta-create",
        }
    }

    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }

    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, agfs::config::Perm)> {
        workloads::allow_rw_rules(session_root)
    }

    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        run_create(dest, self.count)
    }
}
