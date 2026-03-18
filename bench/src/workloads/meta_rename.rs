use crate::workload::{Workload, WorkloadKind};
use crate::workloads;
use crate::workloads::meta_shared::{self, MetaSource};
use anyhow::Result;
use std::path::Path;

pub struct MetaRename {
    pub source: MetaSource,
}

impl MetaRename {
    pub fn all() -> Vec<Self> {
        MetaSource::ALL
            .iter()
            .map(|&s| Self { source: s })
            .collect()
    }
}

pub fn details() -> workloads::WorkloadDetails {
    workloads::workload_details(
        "Rename 10,000 files, measuring per-operation latency.",
        "Fixture varies by source: base populates before mount; stage creates inside the mount; checkpoint snapshots after creation.",
        None,
        &meta_shared::meta_rename_execution(),
        file!(),
    )
}

impl Workload for MetaRename {
    fn name(&self) -> &'static str {
        match self.source {
            MetaSource::Base => "meta-rename-base",
            MetaSource::Stage => "meta-rename-stage",
            MetaSource::Checkpoint => "meta-rename-checkpoint",
        }
    }
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }
    fn description(&self) -> &'static str {
        match self.source {
            MetaSource::Base => "Rename 10,000 base-layer files",
            MetaSource::Stage => "Rename 10,000 stage-local files",
            MetaSource::Checkpoint => "Rename 10,000 checkpoint-layer files",
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
        meta_shared::populate_files_for_source(self.source, base_work_dir)
    }
    fn prepare_workdir(&self, dest: &Path) -> Result<()> {
        meta_shared::prepare_files_for_source(self.source, dest)
    }
    fn needs_prepare_workdir(&self) -> bool {
        meta_shared::needs_prepare(self.source)
    }
    fn needs_checkpoint(&self) -> bool {
        meta_shared::needs_checkpoint(self.source)
    }
    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        meta_shared::run_meta_rename(dest)
    }
}
