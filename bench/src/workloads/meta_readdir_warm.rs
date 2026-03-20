use crate::workload::{Workload, WorkloadKind};
use crate::workloads;
use crate::workloads::meta_shared::{self, MetaSource};
use anyhow::Result;
use std::path::Path;

pub struct MetaReaddirWarm {
    pub source: MetaSource,
    pub count: usize,
}

impl MetaReaddirWarm {
    pub fn all() -> Vec<Self> {
        let mut v = Vec::new();
        for &count in &[meta_shared::LARGE_DIR, meta_shared::SMALL_DIR] {
            for &source in &MetaSource::ALL {
                v.push(Self { source, count });
            }
        }
        v
    }
}

pub fn details() -> workloads::WorkloadDetails {
    workloads::workload_details(
        "Enumerate one warm directory containing 100 or 10,000 files, measuring per-readdir latency.",
        "Fixture varies by source: base populates before mount; stage creates inside the mount; checkpoint snapshots after creation.",
        None,
        &meta_shared::meta_readdir_core_execution(),
        file!(),
    )
}

impl Workload for MetaReaddirWarm {
    fn name(&self) -> &'static str {
        match (self.count, self.source) {
            (100, MetaSource::Base) => "meta-readdir-100-base",
            (100, MetaSource::Stage) => "meta-readdir-100-stage",
            (100, MetaSource::Checkpoint) => "meta-readdir-100-checkpoint",
            (_, MetaSource::Base) => "meta-readdir-base",
            (_, MetaSource::Stage) => "meta-readdir-stage",
            (_, MetaSource::Checkpoint) => "meta-readdir-checkpoint",
        }
    }
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }
    fn description(&self) -> &'static str {
        match (self.count, self.source) {
            (100, MetaSource::Base) => {
                "Warm readdir over one base-layer directory with 100 files (small dir)"
            }
            (100, MetaSource::Stage) => {
                "Warm readdir over one stage-local directory with 100 files (small dir)"
            }
            (100, MetaSource::Checkpoint) => {
                "Warm readdir over one checkpoint-layer directory with 100 files (small dir)"
            }
            (_, MetaSource::Base) => {
                "Warm readdir over one base-layer directory with 10,000 files (large dir)"
            }
            (_, MetaSource::Stage) => {
                "Warm readdir over one stage-local directory with 10,000 files (large dir)"
            }
            (_, MetaSource::Checkpoint) => {
                "Warm readdir over one checkpoint-layer directory with 10,000 files (large dir)"
            }
        }
    }
    fn work_dir(&self) -> &'static str {
        self.name()
    }
    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }
    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, agfs::config::Perm)> {
        workloads::allow_rw_rules(session_root)
    }
    fn populate_base(&self, base_work_dir: &Path) -> Result<()> {
        meta_shared::populate_readdir_for_source(self.source, base_work_dir, self.count)
    }
    fn prepare_workdir(&self, dest: &Path) -> Result<()> {
        meta_shared::prepare_readdir_for_source(self.source, dest, self.count)
    }
    fn needs_prepare_workdir(&self) -> bool {
        meta_shared::needs_prepare(self.source)
    }
    fn needs_checkpoint(&self) -> bool {
        meta_shared::needs_checkpoint(self.source)
    }
    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        meta_shared::run_meta_readdir(dest, self.count)
    }
}
