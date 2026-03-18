use crate::workload::{Workload, WorkloadKind};
use crate::workloads;
use crate::workloads::meta_shared::{self, MetaSource};
use anyhow::Result;
use std::path::Path;

pub struct MetaReaddirWarmBase;
pub struct MetaReaddirWarmStage;
pub struct MetaReaddirWarmCheckpoint;

pub fn details_base() -> workloads::WorkloadDetails {
    workloads::workload_details(
        "Op benchmark for warm directory enumeration over a base-layer 1,000x10 tree.",
        "Populates a 1,000-directory base-layer tree with 10 files per directory before timing, then performs one warm-up `read_dir` pass over each directory.",
        None,
        &meta_shared::execution_stub(
            "crate::workloads::meta_shared::run_meta_readdir_warm(dest)?;",
        ),
        file!(),
    )
}

pub fn details_stage() -> workloads::WorkloadDetails {
    workloads::workload_details(
        "Op benchmark for warm directory enumeration over a stage-local 1,000x10 tree.",
        "Creates a 1,000-directory stage-local tree with 10 files per directory before timing, then performs one warm-up `read_dir` pass over each directory.",
        None,
        &meta_shared::execution_stub(
            "crate::workloads::meta_shared::run_meta_readdir_warm(dest)?;",
        ),
        file!(),
    )
}

pub fn details_checkpoint() -> workloads::WorkloadDetails {
    workloads::workload_details(
        "Op benchmark for warm directory enumeration over a checkpoint-layer 1,000x10 tree.",
        "Populates a 1,000-directory checkpoint-layer tree with 10 files per directory before timing, then performs one warm-up `read_dir` pass over each directory.",
        None,
        &meta_shared::execution_stub(
            "crate::workloads::meta_shared::run_meta_readdir_warm(dest)?;",
        ),
        file!(),
    )
}

fn realistic_rules(session_root: &Path) -> Vec<(String, agfs::config::Perm)> {
    workloads::allow_rw_rules(session_root)
}

fn run(dest: &Path) -> Result<()> {
    meta_shared::run_meta_readdir_warm(dest)
}

impl Workload for MetaReaddirWarmBase {
    fn name(&self) -> &'static str {
        "meta-readdir-warm-base"
    }
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }
    fn description(&self) -> &'static str {
        "Warm readdir over a base-layer 1,000-dir tree"
    }
    fn work_dir(&self) -> &'static str {
        "meta-readdir-warm-base"
    }
    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }
    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, agfs::config::Perm)> {
        realistic_rules(session_root)
    }
    fn populate_base(&self, base_work_dir: &Path) -> Result<()> {
        meta_shared::populate_readdir_for_source(MetaSource::Base, base_work_dir)
    }
    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        run(dest)
    }
}

impl Workload for MetaReaddirWarmStage {
    fn name(&self) -> &'static str {
        "meta-readdir-warm-stage"
    }
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }
    fn description(&self) -> &'static str {
        "Warm readdir over a stage-local 1,000-dir tree"
    }
    fn work_dir(&self) -> &'static str {
        "meta-readdir-warm-stage"
    }
    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }
    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, agfs::config::Perm)> {
        realistic_rules(session_root)
    }
    fn prepare_workdir(&self, dest: &Path) -> Result<()> {
        meta_shared::prepare_readdir_for_source(MetaSource::Stage, dest)
    }
    fn needs_prepare_workdir(&self) -> bool {
        meta_shared::needs_prepare(MetaSource::Stage)
    }
    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        run(dest)
    }
}

impl Workload for MetaReaddirWarmCheckpoint {
    fn name(&self) -> &'static str {
        "meta-readdir-warm-checkpoint"
    }
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }
    fn description(&self) -> &'static str {
        "Warm readdir over a checkpoint-layer 1,000-dir tree"
    }
    fn work_dir(&self) -> &'static str {
        "meta-readdir-warm-checkpoint"
    }
    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }
    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, agfs::config::Perm)> {
        realistic_rules(session_root)
    }
    fn prepare_workdir(&self, dest: &Path) -> Result<()> {
        meta_shared::prepare_readdir_for_source(MetaSource::Checkpoint, dest)
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
