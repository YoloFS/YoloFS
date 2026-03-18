use crate::workload::{Workload, WorkloadKind};
use crate::workloads;
use anyhow::Context;
use anyhow::Result;
use std::fs::File;
use std::path::Path;
use std::time::Instant;

pub struct MetaCreate;

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
        "Op benchmark for file creation throughput and latency across 10,000 empty-file creates.",
        "No pre-existing files required. The work directory is created on demand inside the mounted session.",
        None,
        &meta_create_execution(),
        file!(),
    )
}

impl Workload for MetaCreate {
    fn name(&self) -> &'static str {
        "meta-create"
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }

    fn description(&self) -> &'static str {
        "Create 10,000 empty files (file creation throughput)"
    }

    fn work_dir(&self) -> &'static str {
        "meta-create"
    }

    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }

    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, agfs::config::Perm)> {
        workloads::allow_rw_rules(session_root)
    }

    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        run_meta_create(dest)
    }
}
