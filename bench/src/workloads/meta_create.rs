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
        File::create(dest.join(format!("f-{i:06}.dat")))
            .with_context(|| format!("creating f-{i:06}.dat"))?;
        latencies.push(t0.elapsed());
    }

    workloads::emit_op_result(&workloads::summarize_latencies(
        latencies,
        total.elapsed(),
        None,
    ))
}

// The execution display delegates to run_create() which is a plain function.
// We show the core loop body for the report.
fn meta_create_execution() -> String {
    workloads::rust_execution(
        "for i in 0..count {\n\
         \x20   let t0 = Instant::now();\n\
         \x20   File::create(dest.join(format!(\"f-{i:06}.dat\")))?;\n\
         \x20   latencies.push(t0.elapsed());\n\
         }",
    )
}

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
            100 => "meta-create-100",
            100_000 => "meta-create-100k",
            _ => "meta-create",
        }
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }

    fn description(&self) -> &'static str {
        match self.count {
            100 => "Create 100 empty files (small directory)",
            100_000 => "Create 100,000 empty files (stress directory)",
            _ => "Create 10,000 empty files (large directory)",
        }
    }

    fn work_dir(&self) -> &'static str {
        self.name()
    }

    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }

    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, yolofs::config::Perm)> {
        workloads::allow_rw_rules(session_root)
    }

    fn hidden(&self) -> bool {
        self.count == 100_000
    }

    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        run_create(dest, self.count)
    }
}
