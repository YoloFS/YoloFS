// agfs CLI — timeline_cmd.rs
//
// `agfs timeline` — show checkpoint/restore DAG with unreachable branches dimmed.

use crate::journal;
use crate::journal::timeline::Timeline;
use colored::Colorize;
use std::collections::HashMap;

/// Display the checkpoint/restore timeline (full DAG, unreachable dimmed).
pub fn run() -> anyhow::Result<()> {
    let agfs = crate::utils::session_dir()?;
    let records = journal::read(&agfs)?.records;

    let timeline = Timeline::new(records);

    // Collect checkpoint names by gen for restore display.
    let chk_names: HashMap<u64, &str> = timeline
        .all_records()
        .iter()
        .filter_map(|r| match r {
            journal::Record::Checkpoint(c) => Some((c.gen_id, c.name.as_str())),
            _ => None,
        })
        .collect();

    let events: Vec<(usize, &journal::Record)> = timeline
        .all_records()
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            matches!(
                r,
                journal::Record::Checkpoint(_) | journal::Record::Restore { .. }
            )
        })
        .collect();

    if events.is_empty() {
        println!("{}", "No checkpoints.".yellow());
        return Ok(());
    }

    for (i, rec) in &events {
        let reachable = timeline.is_reachable(*i);
        let line = match rec {
            journal::Record::Checkpoint(c) => {
                format!(
                    "{} {}",
                    format!("checkpoint [{}]", c.gen_id).cyan().bold(),
                    c.name.dimmed(),
                )
            }
            journal::Record::Restore {
                gen_id, target_gen, ..
            } => {
                let target_name = chk_names.get(target_gen).copied().unwrap_or("(unknown)");
                format!(
                    "{} {}",
                    format!("restore    [{gen_id}]").yellow().bold(),
                    format!("restored to [{target_gen}] {target_name}").dimmed(),
                )
            }
            _ => unreachable!(),
        };
        if reachable {
            println!("  {line}");
        } else {
            println!("{} {}", "~".dimmed(), line.dimmed());
        }
    }

    Ok(())
}
