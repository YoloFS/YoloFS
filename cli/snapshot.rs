// agfs CLI — snapshot.rs
//
// `agfs snapshot [name]` — create a snapshot.
// `agfs snapshot list`   — list all snapshots from the journal.

use crate::{ioctl, journal};
use anyhow::{Context, Result};
use colored::Colorize;

/// Create a snapshot with the given name (or a timestamp if empty).
pub fn create(name: Option<&str>) -> Result<()> {
    let agfs = crate::utils::session_dir()?;
    let snap_name = match name {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => default_snap_name(),
    };

    let ctl_file = ioctl::open(&agfs).context("opening ctl for snapshot")?;
    let id = ioctl::create_snapshot(&ctl_file, &snap_name)?;

    eprintln!("{} {} (id {})", "snapshot:".green().bold(), snap_name, id);
    Ok(())
}

/// List all snapshots in the journal.
pub fn list() -> Result<()> {
    let agfs = crate::utils::session_dir()?;
    let records = journal::read(&agfs)?;

    // Collect snapshot positions in a single pass
    let snapshots: Vec<(usize, &journal::Record)> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| matches!(r, journal::Record::Snapshot { .. }))
        .collect();

    if snapshots.is_empty() {
        println!("{}", "No snapshots.".yellow());
        return Ok(());
    }

    for (i, &(rec_idx, snap)) in snapshots.iter().enumerate() {
        let journal::Record::Snapshot { id, name } = snap else {
            unreachable!()
        };

        let prev_end = if i > 0 { snapshots[i - 1].0 + 1 } else { 0 };
        let changes = records[prev_end..rec_idx]
            .iter()
            .filter(|r| !matches!(r, journal::Record::Snapshot { .. }))
            .count();

        println!(
            "  {} {} ({} change{})",
            format!("snapshot [{id}]").cyan().bold(),
            name.dimmed(),
            changes,
            crate::utils::plural(changes)
        );
    }

    // Count changes after the last snapshot
    let last_idx = snapshots.last().unwrap().0;
    let trailing = records[last_idx + 1..]
        .iter()
        .filter(|r| !matches!(r, journal::Record::Snapshot { .. }))
        .count();
    if trailing > 0 {
        println!(
            "  {} ({} uncommitted change{} since last snapshot)",
            "(current)".dimmed(),
            trailing,
            crate::utils::plural(trailing)
        );
    }

    Ok(())
}

fn default_snap_name() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as libc::time_t;
    let mut tm = unsafe { std::mem::zeroed::<libc::tm>() };
    unsafe { libc::localtime_r(&secs, &mut tm) };
    format!(
        "snap-{:04}{:02}{:02}-{:02}{:02}{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
    )
}
