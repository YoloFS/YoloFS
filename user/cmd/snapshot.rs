// yolo CLI — snapshot.rs
//
// `yolo snapshot [name]` — create a snapshot.

use crate::ioctl;
use anyhow::{Context, Result};
use colored::Colorize;

/// Create a snapshot with the given name (or a timestamp if empty). With
/// `if_changed`, the kernel skips the snapshot when nothing is staged. With
/// `quiet`, the creation line isn't printed (e.g. when the caller will show
/// `yolo status` next, which already lists the snapshot marker).
pub fn create(name: Option<&str>, if_changed: bool, quiet: bool) -> Result<()> {
    let yolofs = crate::utils::session_dir()?;
    let chk_name = match name {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => default_name(),
    };

    let ctl_file = ioctl::open(&yolofs).context("opening ctl for snapshot")?;
    let flags = if if_changed {
        ioctl::YOLO_SNAPSHOT_IF_CHANGED
    } else {
        0
    };
    let gen_id = ioctl::snapshot(&ctl_file, &chk_name, flags)?;
    if quiet || (if_changed && gen_id == 0) {
        // Either silenced by the caller, or an --if-changed skip (a no-op).
        return Ok(());
    }

    eprintln!(
        "{} {}",
        format!("snapshot [{gen_id}]").cyan().bold(),
        chk_name.dimmed()
    );
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
