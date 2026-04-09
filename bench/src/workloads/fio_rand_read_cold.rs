use crate::workload::{CacheMode, Workload, WorkloadKind};
use crate::workloads::{self, FioSpec};
use anyhow::Result;
use std::path::Path;

pub struct FioRandReadCold;

pub fn spec() -> FioSpec {
    FioSpec {
        name: "fio-rand-read-cold",
        rw: "randread",
        warm_cache: false,
        seed_existing_file: true,
        mix_read_percent: None,
        io_size: None,
    }
}

pub fn details() -> workloads::WorkloadDetails {
    workloads::fio_workload_details(
        "Random buffered read benchmark with cold page cache.",
        file!(),
        spec(),
    )
}

impl Workload for FioRandReadCold {
    fn name(&self) -> &'static str {
        "fio-rand-read-cold"
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }

    fn description(&self) -> &'static str {
        "Random 4K read, 1 GB file, cold page cache (fio)"
    }

    fn work_dir(&self) -> &'static str {
        "fio-rand-read-cold"
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

    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, yolofs::config::Perm)> {
        workloads::allow_rw_rules(session_root)
    }

    fn cache_mode(&self) -> CacheMode {
        CacheMode::DropPageCache
    }

    fn run(&self, dest: &Path, verbose: bool) -> Result<()> {
        workloads::run_fio(spec(), dest, verbose)
    }
}
