// agfs CLI — checkpoint.rs
//
// `agfs checkpoint [name]` — create a checkpoint.
// `agfs checkpoint list`   — list all checkpoints from the journal.

use crate::{ioctl, journal};
use anyhow::{Context, Result};
use colored::Colorize;

/// Create a checkpoint with the given name (or a timestamp if empty).
pub fn create(name: Option<&str>) -> Result<()> {
    let agfs = crate::utils::session_dir()?;
    let chk_name = match name {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => default_name(),
    };

    let ctl_file = ioctl::open(&agfs).context("opening ctl for checkpoint")?;
    let gen_id = ioctl::create_checkpoint(&ctl_file, &chk_name)?;

    eprintln!(
        "{} {}",
        format!("checkpoint [{gen_id}]").cyan().bold(),
        chk_name.dimmed()
    );
    Ok(())
}

/// List all checkpoints and restore events in the journal.
/// Reads the full journal (no extract_live) to show the complete audit trail.
pub fn list() -> Result<()> {
    let agfs = crate::utils::session_dir()?;
    let records = journal::read(&agfs)?.records;

    // Collect checkpoint names by gen for restore display.
    let chk_names: std::collections::HashMap<u64, &str> = records
        .iter()
        .filter_map(|r| match r {
            journal::Record::Checkpoint(c) => Some((c.gen_id, c.name.as_str())),
            _ => None,
        })
        .collect();

    let events: Vec<&journal::Record> = records
        .iter()
        .filter(|r| {
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

    for rec in &events {
        match rec {
            journal::Record::Checkpoint(c) => {
                println!(
                    "{} {}",
                    format!("checkpoint [{}]", c.gen_id).cyan().bold(),
                    c.name.dimmed(),
                );
            }
            journal::Record::Restore {
                gen_id, target_gen, ..
            } => {
                let target_name = chk_names
                    .get(target_gen)
                    .copied()
                    .unwrap_or("(unknown)");
                println!(
                    "{} {}",
                    format!("restore    [{gen_id}]").yellow().bold(),
                    format!("restored to [{target_gen}] {target_name}").dimmed(),
                );
            }
            _ => {}
        }
    }

    Ok(())
}

fn default_name() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as libc::time_t;
    let mut tm = unsafe { std::mem::zeroed::<libc::tm>() };
    unsafe { libc::localtime_r(&secs, &mut tm) };
    format!(
        "chk-{:04}{:02}{:02}-{:02}{:02}{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
    )
}
