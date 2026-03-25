// commit-scaling: a workload that reads COMMIT_SCALING_OP and
// COMMIT_SCALING_COUNT from environment variables to determine what
// operation to run and at what file count. Used by the commit-scaling
// subcommand which sets these vars before calling backend.run_one().

use crate::workload::{Workload, WorkloadKind};
use agfs::config::Perm;
use anyhow::{Context, Result};
use std::path::Path;

pub struct CommitScaling;

impl Workload for CommitScaling {
    fn name(&self) -> &'static str {
        "commit-scaling"
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Micro
    }

    fn description(&self) -> &'static str {
        "Commit scaling data point (op and count from env)"
    }

    fn work_dir(&self) -> &'static str {
        "commit-scaling"
    }

    fn ensure_fixture(&self) -> Result<()> {
        Ok(())
    }

    fn realistic_rules(&self, root: &Path) -> Vec<(String, Perm)> {
        vec![(root.to_string_lossy().into_owned(), Perm::AllowRw)]
    }

    fn hidden(&self) -> bool {
        true
    }

    fn populate_base(&self, base: &Path) -> Result<()> {
        let op = std::env::var("COMMIT_SCALING_OP").unwrap_or_default();
        let count: usize = std::env::var("COMMIT_SCALING_COUNT")
            .unwrap_or_default()
            .parse()
            .unwrap_or(0);
        if needs_base(&op) && count > 0 {
            crate::workloads::populate_files(base, count, 4096)
        } else {
            Ok(())
        }
    }

    fn run(&self, dest: &Path, _verbose: bool) -> Result<()> {
        let op = std::env::var("COMMIT_SCALING_OP").context("COMMIT_SCALING_OP not set")?;
        let count: usize = std::env::var("COMMIT_SCALING_COUNT")
            .context("COMMIT_SCALING_COUNT not set")?
            .parse()
            .context("invalid COMMIT_SCALING_COUNT")?;

        match op.as_str() {
            "create" => {
                std::fs::create_dir_all(dest)?;
                let buf = vec![0u8; 4096];
                for i in 0..count {
                    std::fs::write(dest.join(format!("f-{i:06}.dat")), &buf)?;
                }
            }
            "overwrite" => {
                let buf = vec![0xFFu8; 4096];
                for i in 0..count {
                    std::fs::write(dest.join(format!("file-{i:06}.dat")), &buf)?;
                }
            }
            "rename" => {
                for i in 0..count {
                    std::fs::rename(
                        dest.join(format!("file-{i:06}.dat")),
                        dest.join(format!("renamed-{i:06}.dat")),
                    )?;
                }
            }
            "unlink" => {
                for i in 0..count {
                    std::fs::remove_file(dest.join(format!("file-{i:06}.dat")))?;
                }
            }
            _ => anyhow::bail!("unknown commit-scaling op: {op}"),
        }
        Ok(())
    }
}

fn needs_base(op: &str) -> bool {
    matches!(op, "overwrite" | "rename" | "unlink")
}
