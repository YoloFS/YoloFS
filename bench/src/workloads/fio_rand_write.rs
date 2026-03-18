use crate::workload::{Workload, WorkloadKind};
use crate::workloads::{self, FioSpec};
use anyhow::Result;
use std::path::Path;

pub struct FioRandWrite;

pub fn spec() -> FioSpec {
    FioSpec {
        name: "fio-rand-write",
        rw: "randwrite",
        warm_cache: false,
        seed_existing_file: false,
        mix_read_percent: None,
    }
}

pub fn details() -> workloads::WorkloadDetails {
    workloads::fio_workload_details(
        "Random buffered write benchmark over a 1 GiB logical file space.",
        file!(),
        spec(),
    )
}

impl Workload for FioRandWrite {
    fn name(&self) -> &'static str {
        "fio-rand-write"
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }

    fn description(&self) -> &'static str {
        "Random 4K write, 1 GB file (fio)"
    }

    fn work_dir(&self) -> &'static str {
        "fio-rand-write"
    }

    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }

    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, agfs::config::Perm)> {
        workloads::allow_rw_rules(session_root)
    }

    fn run(&self, dest: &Path, verbose: bool) -> Result<()> {
        workloads::run_fio(spec(), dest, verbose)
    }
}
