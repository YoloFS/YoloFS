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
    let id = ioctl::create_checkpoint(&ctl_file, &chk_name)?;

    eprintln!("{} {} (id {})", "checkpoint:".green().bold(), chk_name, id);
    Ok(())
}

/// List all checkpoints in the journal.
pub fn list() -> Result<()> {
    let agfs = crate::utils::session_dir()?;
    let records = journal::read(&agfs)?;

    // Collect checkpoint positions in a single pass
    let checkpoints: Vec<(usize, &journal::Record)> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| matches!(r, journal::Record::Checkpoint { .. }))
        .collect();

    if checkpoints.is_empty() {
        println!("{}", "No checkpoints.".yellow());
        return Ok(());
    }

    for (i, &(rec_idx, rec)) in checkpoints.iter().enumerate() {
        let journal::Record::Checkpoint { id, name } = rec else {
            unreachable!()
        };

        let prev_end = if i > 0 { checkpoints[i - 1].0 + 1 } else { 0 };
        let changes = records[prev_end..rec_idx]
            .iter()
            .filter(|r| !matches!(r, journal::Record::Checkpoint { .. }))
            .count();

        println!(
            "  {} {} ({} change{})",
            format!("checkpoint [{id}]").cyan().bold(),
            name.dimmed(),
            changes,
            crate::utils::plural(changes)
        );
    }

    // Count changes after the last checkpoint
    let last_idx = checkpoints.last().unwrap().0;
    let trailing = records[last_idx + 1..]
        .iter()
        .filter(|r| !matches!(r, journal::Record::Checkpoint { .. }))
        .count();
    if trailing > 0 {
        println!(
            "  {} ({} uncommitted change{} since last checkpoint)",
            "(current)".dimmed(),
            trailing,
            crate::utils::plural(trailing)
        );
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
