// agfs CLI — timeline.rs
//
// `agfs timeline` — show checkpoint/restore DAG with unreachable branches dimmed.

use crate::journal::{self, SegmentedJournal};
use colored::Colorize;

/// Display the checkpoint/restore timeline (full DAG, unreachable dimmed).
pub fn run() -> anyhow::Result<()> {
    let agfs = crate::utils::session_dir()?;
    let sj = SegmentedJournal::new(journal::read(&agfs)?);

    if sj.markers.is_empty() {
        println!("{}", "No checkpoints.".yellow());
        return Ok(());
    }

    let alive = sj.markers.alive_segments(sj.segments.len());

    for (m_idx, marker) in sj.markers.iter().enumerate() {
        let reachable = alive[m_idx];
        let line = match marker {
            journal::Record::Checkpoint { gen_id, name } => {
                format!(
                    "{} {}",
                    format!("checkpoint [{gen_id}]").cyan().bold(),
                    name.dimmed(),
                )
            }
            journal::Record::Restore {
                gen_id, target_gen,
            } => {
                format!(
                    "{} {}",
                    format!("restore    [{gen_id}]").yellow().bold(),
                    format!("restored to [{target_gen}]").dimmed(),
                )
            }
            _ => unreachable!("markers only contain checkpoint/restore records"),
        };
        if reachable {
            println!("  {line}");
        } else {
            println!("  {} {}", line.dimmed(), "(unreachable)".dimmed());
        }
    }

    Ok(())
}
