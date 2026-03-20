use crate::workload::{CacheMode, Workload, WorkloadKind};
use crate::workloads;
use crate::workloads::meta_shared::{self, MetaSource};
use anyhow::Result;
use std::path::Path;

pub struct MetaReaddirCold {
    pub source: MetaSource,
}

impl MetaReaddirCold {
    pub fn all() -> Vec<Self> {
        MetaSource::ALL
            .iter()
            .map(|&s| Self { source: s })
            .collect()
    }
}

pub fn details() -> workloads::WorkloadDetails {
    let mut d = workloads::workload_details(
        "One readdir syscall after dropping page caches, measuring cold directory enumeration latency.",
        "Fixture varies by source: base populates before mount; stage creates inside the mount; checkpoint snapshots after creation.",
        Some("Page cache is dropped after all subprocess setup but before the timed readdir."),
        &meta_shared::meta_readdir_cold_core_execution(),
        file!(),
    );
    d.caveat = Some(meta_shared::COLD_MOUNT_CAVEAT.to_string());
    d
}

impl Workload for MetaReaddirCold {
    fn name(&self) -> &'static str {
        match self.source {
            MetaSource::Base => "meta-readdir-cold-base",
            MetaSource::Stage => "meta-readdir-cold-stage",
            MetaSource::Checkpoint => "meta-readdir-cold-checkpoint",
        }
    }
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }
    fn description(&self) -> &'static str {
        match self.source {
            MetaSource::Base => "One cold readdir over a base-layer 10-entry directory",
            MetaSource::Stage => "One cold readdir over a stage-local 10-entry directory",
            MetaSource::Checkpoint => "One cold readdir over a checkpoint-layer 10-entry directory",
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
        meta_shared::populate_readdir_for_source_cold(self.source, base_work_dir, meta_shared::LARGE_DIR)
    }
    fn prepare_workdir(&self, dest: &Path) -> Result<()> {
        meta_shared::prepare_readdir_for_source_cold(self.source, dest, meta_shared::LARGE_DIR)
    }
    fn needs_prepare_workdir(&self) -> bool {
        meta_shared::needs_prepare(self.source)
    }
    fn needs_checkpoint(&self) -> bool {
        meta_shared::needs_checkpoint(self.source)
    }
    fn cache_mode(&self) -> CacheMode {
        meta_shared::cache_mode(true)
    }
    fn hidden(&self) -> bool {
        true
    }
    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        meta_shared::run_meta_readdir_cold(dest, meta_shared::LARGE_DIR)
    }
}
