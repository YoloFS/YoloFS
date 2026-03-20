// agfs CLI — timeline.rs
//
// `agfs timeline` — show checkpoint/restore DAG with unreachable branches dimmed.

use crate::journal;
use crate::journal::{Marker, SegmentedJournal};
use colored::Colorize;
use std::collections::HashMap;

/// Display the checkpoint/restore timeline (full DAG, unreachable dimmed).
pub fn run() -> anyhow::Result<()> {
    let agfs = crate::utils::session_dir()?;
    let sj = SegmentedJournal::new(journal::read(&agfs)?);

    if sj.markers.is_empty() {
        println!("{}", "No checkpoints.".yellow());
        return Ok(());
    }

    let alive = sj.markers.alive_segments(sj.segments.len());

    // Collect checkpoint names by gen for restore display.
    let chk_names: HashMap<u64, &str> = sj
        .markers
        .iter()
        .filter_map(|m| match m {
            Marker::Checkpoint { checkpoint, .. } => {
                Some((checkpoint.gen_id, checkpoint.name.as_str()))
            }
            _ => None,
        })
        .collect();

    for (m_idx, marker) in sj.markers.iter().enumerate() {
        let reachable = alive[m_idx];
        let line = match marker {
            Marker::Checkpoint { checkpoint, .. } => {
                format!(
                    "{} {}",
                    format!("checkpoint [{}]", checkpoint.gen_id).cyan().bold(),
                    checkpoint.name.dimmed(),
                )
            }
            Marker::Restore {
                gen_id, target_gen, ..
            } => {
                let target_name = chk_names.get(target_gen).copied().unwrap_or("(unknown)");
                format!(
                    "{} {}",
                    format!("restore    [{gen_id}]").yellow().bold(),
                    format!("restored to [{target_gen}] {target_name}").dimmed(),
                )
            }
        };
        if reachable {
            println!("  {line}");
        } else {
            println!("{} {}", "~".dimmed(), line.dimmed());
        }
    }

    Ok(())
}
