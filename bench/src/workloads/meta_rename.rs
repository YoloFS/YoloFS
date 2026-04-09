use crate::workload::{Workload, WorkloadKind};
use crate::workloads;
use crate::workloads::meta_shared::{self, MetaSource};
use anyhow::Result;
use std::path::Path;

pub struct MetaRename {
    pub source: MetaSource,
    pub count: usize,
}

impl MetaRename {
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
        "Rename 10,000 files, measuring per-operation latency.",
        "Fixture varies by source: base populates before mount; stage creates inside the mount; checkpoint snapshots after creation.",
        None,
        &meta_shared::meta_rename_core_execution(),
        file!(),
    )
}

impl Workload for MetaRename {
    fn name(&self) -> &'static str {
        match (self.count, self.source) {
            (100, MetaSource::Base) => "meta-rename-100-base",
            (100, MetaSource::Stage) => "meta-rename-100-stage",
            (100, MetaSource::Checkpoint) => "meta-rename-100-checkpoint",
            (_, MetaSource::Base) => "meta-rename-base",
            (_, MetaSource::Stage) => "meta-rename-stage",
            (_, MetaSource::Checkpoint) => "meta-rename-checkpoint",
        }
    }
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }
    fn description(&self) -> &'static str {
        match (self.count, self.source) {
            (100, MetaSource::Base) => "Rename 100 base-layer files (small dir)",
            (100, MetaSource::Stage) => "Rename 100 stage-local files (small dir)",
            (100, MetaSource::Checkpoint) => "Rename 100 checkpoint-layer files (small dir)",
            (_, MetaSource::Base) => "Rename 10,000 base-layer files (large dir)",
            (_, MetaSource::Stage) => "Rename 10,000 stage-local files (large dir)",
            (_, MetaSource::Checkpoint) => "Rename 10,000 checkpoint-layer files (large dir)",
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
    fn populate_base(&self, base_work_dir: &Path) -> Result<()> {
        meta_shared::populate_files_for_source(self.source, base_work_dir, self.count)
    }
    fn prepare_workdir(&self, dest: &Path) -> Result<()> {
        meta_shared::prepare_files_for_source(self.source, dest, self.count)
    }
    fn needs_prepare_workdir(&self) -> bool {
        meta_shared::needs_prepare(self.source)
    }
    fn needs_checkpoint(&self) -> bool {
        meta_shared::needs_checkpoint(self.source)
    }
    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        meta_shared::run_meta_rename(dest, self.count)
    }
}
