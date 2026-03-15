use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Returns "s" when count != 1, "" otherwise.
pub fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// Convert an agfs-relative path (e.g. "/src/main.rs") to a base filesystem path.
pub fn to_base_path(rel: &str) -> PathBuf {
    Path::new("/").join(rel.trim_start_matches('/'))
}

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
