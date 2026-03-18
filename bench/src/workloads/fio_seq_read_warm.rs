use crate::workload::{Workload, WorkloadKind};
use crate::workloads::{self, FioSpec};
use anyhow::Result;
use std::path::Path;

pub struct FioSeqReadWarm;

pub fn spec() -> FioSpec {
    FioSpec {
        name: "fio-seq-read-warm",
        rw: "read",
        warm_cache: true,
        seed_existing_file: true,
        mix_read_percent: None,
    }
}

pub fn details() -> workloads::WorkloadDetails {
    workloads::fio_workload_details(
        "Sequential buffered read benchmark with warm page cache.",
        file!(),
        spec(),
    )
}

impl Workload for FioSeqReadWarm {
    fn name(&self) -> &'static str {
        "fio-seq-read-warm"
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }

    fn description(&self) -> &'static str {
        "Sequential 4K read, 1 GB file, warm page cache (fio)"
    }

    fn work_dir(&self) -> &'static str {
        "fio-seq-read-warm"
    }

    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }

    fn prepare_workdir(&self, dest: &Path) -> Result<()> {
        workloads::prepare_seeded_fio_workdir(dest)
    }

    fn needs_prepare_workdir(&self) -> bool {
        true
    }

    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, agfs::config::Perm)> {
        workloads::allow_rw_rules(session_root)
    }

    fn run(&self, dest: &Path, verbose: bool) -> Result<()> {
        workloads::run_fio(spec(), dest, verbose)
    }
}
