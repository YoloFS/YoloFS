use crate::workload::{Workload, WorkloadKind};
use crate::workloads;
use crate::workloads::meta_shared::{self, MetaSource};
use anyhow::Result;
use std::path::Path;

pub struct MetaAppendBase;
pub struct MetaAppendStage;
pub struct MetaAppendCheckpoint;

pub fn details_base() -> workloads::WorkloadDetails {
    workloads::workload_details(
        "Op benchmark for append throughput on pre-existing base-layer files.",
        &meta_shared::base_or_stage_fixture(MetaSource::Base, "10,000 files"),
        None,
        &meta_shared::execution_stub("crate::workloads::meta_shared::run_meta_append(dest)?;"),
        file!(),
    )
}

pub fn details_stage() -> workloads::WorkloadDetails {
    workloads::workload_details(
        "Op benchmark for append throughput on stage-local files.",
        &meta_shared::base_or_stage_fixture(MetaSource::Stage, "10,000 files"),
        None,
        &meta_shared::execution_stub("crate::workloads::meta_shared::run_meta_append(dest)?;"),
        file!(),
    )
}

pub fn details_checkpoint() -> workloads::WorkloadDetails {
    workloads::workload_details(
        "Op benchmark for append throughput on checkpoint-layer files.",
        &meta_shared::source_fixture(MetaSource::Checkpoint, "10,000 files"),
        None,
        &meta_shared::execution_stub("crate::workloads::meta_shared::run_meta_append(dest)?;"),
        file!(),
    )
}

fn realistic_rules(session_root: &Path) -> Vec<(String, agfs::config::Perm)> {
    workloads::allow_rw_rules(session_root)
}

fn run(dest: &Path) -> Result<()> {
    meta_shared::run_meta_append(dest)
}

impl Workload for MetaAppendBase {
    fn name(&self) -> &'static str {
        "meta-append-base"
    }
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }
    fn description(&self) -> &'static str {
        "Append 4 KiB to 10,000 base-layer files"
    }
    fn work_dir(&self) -> &'static str {
        "meta-append-base"
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

impl Workload for MetaAppendStage {
    fn name(&self) -> &'static str {
        "meta-append-stage"
    }
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }
    fn description(&self) -> &'static str {
        "Append 4 KiB to 10,000 stage-local files"
    }
    fn work_dir(&self) -> &'static str {
        "meta-append-stage"
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

impl Workload for MetaAppendCheckpoint {
    fn name(&self) -> &'static str {
        "meta-append-checkpoint"
    }
    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Op
    }
    fn description(&self) -> &'static str {
        "Append 4 KiB to 10,000 checkpoint-layer files"
    }
    fn work_dir(&self) -> &'static str {
        "meta-append-checkpoint"
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
