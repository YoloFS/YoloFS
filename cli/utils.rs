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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn plural_zero() {
        assert_eq!(plural(0), "s");
    }

    #[test]
    fn plural_one() {
        assert_eq!(plural(1), "");
    }

    #[test]
    fn plural_two() {
        assert_eq!(plural(2), "s");
    }

    #[test]
    fn to_base_path_with_leading_slash() {
        assert_eq!(to_base_path("/foo/bar"), PathBuf::from("/foo/bar"));
    }

    #[test]
    fn to_base_path_without_leading_slash() {
        assert_eq!(to_base_path("foo/bar"), PathBuf::from("/foo/bar"));
    }

    #[test]
    fn to_base_path_root() {
        assert_eq!(to_base_path("/"), PathBuf::from("/"));
    }

    #[test]
    fn session_dir_from_env() {
        let dir = "/tmp/agfs-test-session";
        // SAFETY: test is single-threaded; no other thread reads this var.
        unsafe { std::env::set_var("AGFS_SESSION", dir); }
        let result = session_dir().unwrap();
        assert_eq!(result, PathBuf::from(dir));
        unsafe { std::env::remove_var("AGFS_SESSION"); }
    }

    #[test]
    fn session_dir_from_dotdir() {
        // SAFETY: test is single-threaded; no other thread reads this var.
        unsafe { std::env::remove_var("AGFS_SESSION"); }
        let tmp = tempfile::tempdir().unwrap();
        let agfs_dir = tmp.path().join(".agfs");
        std::fs::create_dir(&agfs_dir).unwrap();
        // Change cwd into the tempdir so session_dir finds .agfs/
        let orig = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        let result = session_dir().unwrap();
        assert_eq!(result, agfs_dir);
        std::env::set_current_dir(orig).unwrap();
    }
}
