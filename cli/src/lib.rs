pub mod abort;
pub mod commit;
pub mod config;
pub mod ioctl;
pub mod diff;
pub mod journal;
pub mod mount;
pub mod exec;
pub mod status;
pub mod unmount;
pub mod watch;

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Locate the agfs session directory.
/// Checks AGFS_SESSION env var first, then falls back to .agfs/.
pub fn session_dir() -> Result<PathBuf> {
    if let Ok(session) = std::env::var("AGFS_SESSION") {
        return Ok(PathBuf::from(session));
    }
    let cwd = std::env::current_dir().context("getting cwd")?;
    let dir = cwd.join(".agfs");
    if dir.exists() {
        Ok(dir)
    } else {
        anyhow::bail!("no agfs session found (no .agfs/ directory)")
    }
}
