use crate::workload::{Workload, WorkloadKind};
use crate::workloads;
use crate::workloads::meta_shared::{self, MetaSource};
use anyhow::Result;
use std::path::Path;

pub struct MetaStatWarmBase;
pub struct MetaStatWarmStage;
pub struct MetaStatWarmCheckpoint;

pub fn details_base() -> workloads::WorkloadDetails {
    workloads::workload_details(
        "Op benchmark for warm metadata lookup on a base-layer 10,000-file fixture.",
        "Populates 10,000 base-layer files before timing, then pre-stats them once to warm dcache/icache.",
        None,
        &meta_shared::execution_stub("crate::workloads::meta_shared::run_meta_stat_warm(dest)?;"),
        file!(),
    )
}

pub fn details_stage() -> workloads::WorkloadDetails {
    workloads::workload_details(
        "Op benchmark for warm metadata lookup on a stage-local 10,000-file fixture.",
        "Creates 10,000 stage-local files before timing, then pre-stats them once to warm dcache/icache.",
        None,
        &meta_shared::execution_stub("crate::workloads::meta_shared::run_meta_stat_warm(dest)?;"),
        file!(),
    )
}

pub fn details_checkpoint() -> workloads::WorkloadDetails {
    workloads::workload_details(
        "Op benchmark for warm metadata lookup on a checkpoint-layer 10,000-file fixture.",
        "Populates 10,000 checkpoint-layer files before timing, then pre-stats them once to warm dcache/icache.",
        None,
        &meta_shared::execution_stub("crate::workloads::meta_shared::run_meta_stat_warm(dest)?;"),
        file!(),
    )
}

fn realistic_rules(session_root: &Path) -> Vec<(String, agfs::config::Perm)> {
    workloads::allow_rw_rules(session_root)
}

fn run(dest: &Path) -> Result<()> {
    meta_shared::run_meta_stat_warm(dest)
}

impl Workload for MetaStatWarmBase {
    fn name(&self) -> &'static str {
        "meta-stat-warm-base"
    }
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }
    fn description(&self) -> &'static str {
        "Warm stat over 10,000 base-layer files"
    }
    fn work_dir(&self) -> &'static str {
        "meta-stat-warm-base"
    }
    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }
    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, agfs::config::Perm)> {
        realistic_rules(session_root)
    }
    fn populate_base(&self, base_work_dir: &Path) -> Result<()> {
        meta_shared::populate_files_for_source(MetaSource::Base, base_work_dir)
    }
    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        run(dest)
    }
}

impl Workload for MetaStatWarmStage {
    fn name(&self) -> &'static str {
        "meta-stat-warm-stage"
    }
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }
    fn description(&self) -> &'static str {
        "Warm stat over 10,000 stage-local files"
    }
    fn work_dir(&self) -> &'static str {
        "meta-stat-warm-stage"
    }
    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }
    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, agfs::config::Perm)> {
        realistic_rules(session_root)
    }
    fn prepare_workdir(&self, dest: &Path) -> Result<()> {
        meta_shared::prepare_files_for_source(MetaSource::Stage, dest)
    }
    fn needs_prepare_workdir(&self) -> bool {
        meta_shared::needs_prepare(MetaSource::Stage)
    }
    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        run(dest)
    }
}

impl Workload for MetaStatWarmCheckpoint {
    fn name(&self) -> &'static str {
        "meta-stat-warm-checkpoint"
    }
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }
    fn description(&self) -> &'static str {
        "Warm stat over 10,000 checkpoint-layer files"
    }
    fn work_dir(&self) -> &'static str {
        "meta-stat-warm-checkpoint"
    }
    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }
    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, agfs::config::Perm)> {
        realistic_rules(session_root)
    }
    fn prepare_workdir(&self, dest: &Path) -> Result<()> {
        meta_shared::prepare_files_for_source(MetaSource::Checkpoint, dest)
    }
    fn needs_prepare_workdir(&self) -> bool {
        meta_shared::needs_prepare(MetaSource::Checkpoint)
    }
    fn needs_checkpoint(&self) -> bool {
        meta_shared::needs_checkpoint(MetaSource::Checkpoint)
    }
    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        run(dest)
    }
}
