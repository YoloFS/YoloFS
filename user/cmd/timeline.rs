// yolo CLI — timeline.rs
//
// `yolo timeline` — show snapshot/travel DAG with unreachable branches dimmed.

use crate::journal::{self, Journal};
use colored::Colorize;

/// Display the snapshot/travel timeline (full DAG, unreachable dimmed).
pub fn run() -> anyhow::Result<()> {
    let yolofs = crate::utils::session_dir()?;
    let journal = Journal::read(&yolofs)?;

    if journal.markers.len() <= 1 {
        crate::report::empty("no snapshots");
        return Ok(());
    }

    for (m_idx, marker) in journal.markers.iter().enumerate() {
        if m_idx == 0 {
            continue;
        }
        let reachable = journal.is_alive(m_idx - 1);
        let line = match marker {
            journal::Marker::Snapshot { name } => {
                format!(
                    "{} {}",
                    format!("snapshot {m_idx}").cyan().bold(),
                    name.dimmed(),
                )
            }
            journal::Marker::Travel { target_gen } => {
                format!(
                    "{} {}",
                    format!("travel   {m_idx}").yellow().bold(),
                    format!("→ {target_gen}").dimmed(),
                )
            }
        };
        if reachable {
            println!("  {line}");
        } else {
            println!("  {} {}", line.dimmed(), "(unreachable)".dimmed());
        }
    }

    Ok(())
}
