// agfs CLI — checkpoint.rs
//
// `agfs checkpoint [name]` — create a checkpoint.

use crate::ioctl;
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
    let gen_id = ioctl::create_checkpoint(&ctl_file, &chk_name, 0)?;

    eprintln!(
        "{} {}",
        format!("checkpoint [{gen_id}]").cyan().bold(),
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
