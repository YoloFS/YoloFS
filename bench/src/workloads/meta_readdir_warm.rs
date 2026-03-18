use crate::workload::{Workload, WorkloadKind};
use crate::workloads;
use crate::workloads::meta_shared::{self, MetaSource};
use anyhow::Result;
use std::path::Path;

pub struct MetaReaddirWarm {
    pub source: MetaSource,
}

impl MetaReaddirWarm {
    pub fn all() -> Vec<Self> {
        MetaSource::ALL
            .iter()
            .map(|&s| Self { source: s })
            .collect()
    }
}

pub fn details() -> workloads::WorkloadDetails {
    workloads::workload_details(
        "Enumerate 1,000 directories (10 files each) from warm dcache, measuring per-directory latency.",
        "Fixture varies by source: base populates before mount; stage creates inside the mount; checkpoint snapshots after creation.",
        None,
        &meta_shared::meta_readdir_warm_execution(),
        file!(),
    )
}

impl Workload for MetaReaddirWarm {
    fn name(&self) -> &'static str {
        match self.source {
            MetaSource::Base => "meta-readdir-warm-base",
            MetaSource::Stage => "meta-readdir-warm-stage",
            MetaSource::Checkpoint => "meta-readdir-warm-checkpoint",
        }
    }
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }
    fn description(&self) -> &'static str {
        match self.source {
            MetaSource::Base => "Warm readdir over a base-layer 1,000-dir tree",
            MetaSource::Stage => "Warm readdir over a stage-local 1,000-dir tree",
            MetaSource::Checkpoint => "Warm readdir over a checkpoint-layer 1,000-dir tree",
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
        meta_shared::populate_readdir_for_source(self.source, base_work_dir)
    }
    fn prepare_workdir(&self, dest: &Path) -> Result<()> {
        meta_shared::prepare_readdir_for_source(self.source, dest)
    }
    fn needs_prepare_workdir(&self) -> bool {
        meta_shared::needs_prepare(self.source)
    }
    fn needs_checkpoint(&self) -> bool {
        meta_shared::needs_checkpoint(self.source)
    }
    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        meta_shared::run_meta_readdir_warm(dest)
    }
}
