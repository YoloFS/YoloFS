pub mod worktree;
pub mod write_files;

use crate::workload::Workload;

/// All registered workloads, in the order they appear in reports.
pub fn all() -> Vec<Box<dyn Workload>> {
    vec![
        Box::new(worktree::Worktree::new()),
        Box::new(write_files::WriteFiles::new()),
    ]
}

pub fn by_name(name: &str) -> Option<Box<dyn Workload>> {
    all().into_iter().find(|w| w.name() == name)
}
