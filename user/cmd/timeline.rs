// agfs CLI — timeline.rs
//
// `agfs timeline` — show checkpoint/restore DAG with unreachable branches dimmed.

use crate::journal::{self, Journal};
use colored::Colorize;

/// Display the checkpoint/restore timeline (full DAG, unreachable dimmed).
pub fn run() -> anyhow::Result<()> {
    let agfs = crate::utils::session_dir()?;
    let journal = Journal::read(&agfs)?;

    if journal.metas.len() <= 1 {
        println!("{}", "No checkpoints.".yellow());
        return Ok(());
    }

    for (m_idx, meta) in journal.metas.iter().enumerate() {
        if m_idx == 0 {
            continue;
        }
        let reachable = journal.is_alive(m_idx - 1);
        let line = match meta {
            journal::Meta::Mark { gen_id, name } => {
                format!(
                    "{} {}",
                    format!("checkpoint [{gen_id}]").cyan().bold(),
                    name.dimmed(),
                )
            }
            journal::Meta::Jump { gen_id, target_gen } => {
                format!(
                    "{} {}",
                    format!("restore    [{gen_id}]").yellow().bold(),
                    format!("restored to [{target_gen}]").dimmed(),
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
