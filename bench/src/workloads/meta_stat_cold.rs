use crate::workload::{Workload, WorkloadKind};
use crate::workloads;
use crate::workloads::meta_shared::{self, MetaSource};
use anyhow::Result;
use std::path::Path;

pub struct MetaStatColdBase;
pub struct MetaStatColdStage;
pub struct MetaStatColdCheckpoint;

pub fn details_base() -> workloads::WorkloadDetails {
    let mut d = workloads::workload_details(
        "Op benchmark for one cold metadata lookup on a base-layer 10,000-file fixture.",
        &meta_shared::base_or_stage_fixture(MetaSource::Base, "10,000 files"),
        Some(
            "Parent/backend runner drops the Linux page cache before spawning the workload subprocess.",
        ),
        &meta_shared::execution_stub("crate::workloads::meta_shared::run_meta_stat_cold(dest)?;"),
        file!(),
    );
    d.caveat = Some(meta_shared::COLD_MOUNT_CAVEAT.to_string());
    d
}

pub fn details_stage() -> workloads::WorkloadDetails {
    let mut d = workloads::workload_details(
        "Op benchmark for one cold metadata lookup on a stage-local 10,000-file fixture.",
        &meta_shared::base_or_stage_fixture(MetaSource::Stage, "10,000 files"),
        Some(
            "Parent/backend runner drops the Linux page cache after stage-local fixture creation and before spawning the workload subprocess.",
        ),
        &meta_shared::execution_stub("crate::workloads::meta_shared::run_meta_stat_cold(dest)?;"),
        file!(),
    );
    d.caveat = Some(meta_shared::COLD_MOUNT_CAVEAT.to_string());
    d
}

pub fn details_checkpoint() -> workloads::WorkloadDetails {
    let mut d = workloads::workload_details(
        "Op benchmark for one cold metadata lookup on a checkpoint-layer 10,000-file fixture.",
        &meta_shared::source_fixture(MetaSource::Checkpoint, "10,000 files"),
        Some(
            "Parent/backend runner drops the Linux page cache after checkpoint and before spawning the workload subprocess.",
        ),
        &meta_shared::execution_stub("crate::workloads::meta_shared::run_meta_stat_cold(dest)?;"),
        file!(),
    );
    d.caveat = Some(meta_shared::COLD_MOUNT_CAVEAT.to_string());
    d
}

fn realistic_rules(session_root: &Path) -> Vec<(String, agfs::config::Perm)> {
    workloads::allow_rw_rules(session_root)
}

fn run(dest: &Path) -> Result<()> {
    meta_shared::run_meta_stat_cold(dest)
}

impl Workload for MetaStatColdBase {
    fn name(&self) -> &'static str {
        "meta-stat-cold-base"
    }
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }
    fn description(&self) -> &'static str {
        "One cold stat against a 10,000-file base-layer fixture"
    }
    fn work_dir(&self) -> &'static str {
        "meta-stat-cold-base"
    }
    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }
    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, agfs::config::Perm)> {
        realistic_rules(session_root)
    }
    fn populate_base(&self, base_work_dir: &Path) -> Result<()> {
        meta_shared::populate_files_for_source_cold(MetaSource::Base, base_work_dir)
    }
    fn cache_mode(&self) -> crate::workload::CacheMode {
        meta_shared::cache_mode(true)
    }
    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        run(dest)
    }
}

impl Workload for MetaStatColdStage {
    fn name(&self) -> &'static str {
        "meta-stat-cold-stage"
    }
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }
    fn description(&self) -> &'static str {
        "One cold stat against a 10,000-file stage-local fixture"
    }
    fn work_dir(&self) -> &'static str {
        "meta-stat-cold-stage"
    }
    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }
    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, agfs::config::Perm)> {
        realistic_rules(session_root)
    }
    fn prepare_workdir(&self, dest: &Path) -> Result<()> {
        meta_shared::prepare_files_for_source_cold(MetaSource::Stage, dest)
    }
    fn needs_prepare_workdir(&self) -> bool {
        meta_shared::needs_prepare(MetaSource::Stage)
    }
    fn cache_mode(&self) -> crate::workload::CacheMode {
        meta_shared::cache_mode(true)
    }
    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        run(dest)
    }
}

impl Workload for MetaStatColdCheckpoint {
    fn name(&self) -> &'static str {
        "meta-stat-cold-checkpoint"
    }
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }
    fn description(&self) -> &'static str {
        "One cold stat against a 10,000-file checkpoint-layer fixture"
    }
    fn work_dir(&self) -> &'static str {
        "meta-stat-cold-checkpoint"
    }
    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }
    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, agfs::config::Perm)> {
        realistic_rules(session_root)
    }
    fn prepare_workdir(&self, dest: &Path) -> Result<()> {
        meta_shared::prepare_files_for_source_cold(MetaSource::Checkpoint, dest)
    }
    fn needs_prepare_workdir(&self) -> bool {
        meta_shared::needs_prepare(MetaSource::Checkpoint)
    }
    fn needs_checkpoint(&self) -> bool {
        meta_shared::needs_checkpoint(MetaSource::Checkpoint)
    }
    fn cache_mode(&self) -> crate::workload::CacheMode {
        meta_shared::cache_mode(true)
    }
    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        run(dest)
    }
}
