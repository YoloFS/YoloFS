use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Shard size must match YOLO_SHARD_SIZE in kmod/staging.c.
/// Must match YOLO_SHARD_SIZE in kmod/staging.c.
const SHARD_SIZE: u32 = 100;

/// Get the staged inode path for a given ino.
/// Layout: `inodes/<shard>/<ino>` where shard = ino / SHARD_SIZE.
pub fn inode_path(yolo_dir: &Path, ino: u32) -> PathBuf {
    yolo_dir
        .join("inodes")
        .join((ino / SHARD_SIZE).to_string())
        .join(ino.to_string())
}

/// Returns "s" when count != 1, "" otherwise.
pub fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// Convert an yolofs-relative path (e.g. "/src/main.rs") to a base filesystem path.
pub fn to_base_path(rel: &str) -> PathBuf {
    Path::new("/").join(rel.trim_start_matches('/'))
}

/// Normalize a user-supplied path to match the absolute-path format used in the
/// journal. Relative paths are resolved against `cwd`
/// (e.g. "hello.txt" → "<cwd>/hello.txt"). Absolute paths are kept as-is.
pub fn normalize_path_with_cwd(p: &str, cwd: &Path) -> String {
    let stripped = p.strip_prefix("./").unwrap_or(p);
    if stripped.starts_with('/') {
        stripped.to_string()
    } else {
        cwd.join(stripped).to_string_lossy().to_string()
    }
}

/// Convenience wrapper that resolves against the real working directory.
pub fn normalize_path(p: &str) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    normalize_path_with_cwd(p, &cwd)
}

/// Locate the yolofs session directory.
/// Checks YOLO_SESSION env var first, then falls back to .yolofs/.
pub fn session_dir() -> Result<PathBuf> {
    if let Ok(session) = std::env::var("YOLO_SESSION") {
        return Ok(PathBuf::from(session));
    }
    let cwd = std::env::current_dir().context("getting cwd")?;
    let dir = cwd.join(".yolofs");
    if dir.exists() {
        Ok(dir)
    } else {
        anyhow::bail!(
            "no yolofs session in {} — run `yolo mount` first (or `yolo init` to set up a new project)",
            cwd.display()
        )
    }
}

/// Per-user base directory that holds every session's mountpoint. The
/// mountpoint is an empty directory the over-`/` mount lands on — no data is
/// stored there — so it belongs on ephemeral, per-user runtime storage *outside*
/// the workspace, where editors and indexers won't wander into a recursive view
/// of `/`. `/run/user/<uid>` is the systemd per-user runtime dir: tmpfs, mode
/// 0700, wiped on logout. `getuid()` (not euid) is the invoking user even under
/// the setuid-root CLI, so this lands in *their* runtime dir.
fn runtime_base() -> PathBuf {
    let uid = nix::unistd::getuid().as_raw();
    PathBuf::from(format!("/run/user/{uid}/yolofs"))
}

/// Create a fresh, unique mountpoint directory under the per-user runtime base
/// and return it. Called once per `yolo mount`; the chosen path is recorded in
/// the `.yolofs/mnt` symlink, which is the authoritative handle thereafter (see
/// [`mnt_dir`]). Uniqueness comes from `mkdtemp` rather than a derived key — so
/// there's no hashing, no canonicalization, and nothing to recompute. The dir
/// *is* the mount root; no nested `mnt/` is needed since it holds no data.
pub fn create_mnt_dir() -> Result<PathBuf> {
    let base = runtime_base();
    std::fs::create_dir_all(&base)
        .with_context(|| format!("creating runtime base {}", base.display()))?;
    nix::unistd::mkdtemp(&base.join("XXXXXX"))
        .with_context(|| format!("creating a mountpoint under {}", base.display()))
}

/// Resolve where this session's over-`/` mountpoint lives — the single source of
/// truth for the mount root. The location is recorded once at mount time in the
/// `.yolofs/mnt` symlink, so resolving that link is authoritative. When the link
/// isn't there yet (e.g. before setup) this returns the link's own path, which
/// won't exist — so callers correctly see the session as not mounted.
pub fn mnt_dir(yolo_dir: &Path) -> PathBuf {
    let link = yolo_dir.join("mnt");
    std::fs::read_link(&link).unwrap_or(link)
}

/// True if running inside the yolofs mount. `yolo exec` is the only way into the
/// mount and it sets `YOLO_SESSION` for the processes it spawns, so this env var
/// is a reliable (and syscall-free) signal. yolo is a host-side tool — its
/// base-fs operations only work outside — so every subcommand refuses when this
/// is true. The kernel is the real boundary regardless (it refuses
/// gating-changing ioctls from a chrooted caller).
pub fn inside_mount() -> bool {
    std::env::var_os("YOLO_SESSION").is_some()
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

    // session_dir tests mutate global state (env vars + cwd), so they
    // must be serialised.  We use a single test to avoid races with the
    // default parallel test runner.
    #[test]
    fn session_dir_env_and_dotdir() {
        // Ensure we start from a valid cwd (the test runner may have been
        // started from a directory that no longer exists inside the VM).
        let guard = tempfile::tempdir().unwrap();
        std::env::set_current_dir(guard.path()).unwrap();

        // ── Part 1: YOLO_SESSION env var takes precedence ──
        let dir = "/tmp/yolofs-test-session";
        // SAFETY: no other thread reads this var during this test.
        unsafe {
            std::env::set_var("YOLO_SESSION", dir);
        }
        let result = session_dir().unwrap();
        assert_eq!(result, PathBuf::from(dir));
        unsafe {
            std::env::remove_var("YOLO_SESSION");
        }

        // ── Part 2: falls back to .yolofs/ in cwd ──
        let tmp = tempfile::tempdir().unwrap();
        let yolo_dir = tmp.path().join(".yolofs");
        std::fs::create_dir(&yolo_dir).unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        let result = session_dir().unwrap();
        assert_eq!(result, yolo_dir);
    }

    #[test]
    fn normalize_path_relative() {
        let cwd = PathBuf::from("/fake/cwd");
        assert_eq!(
            normalize_path_with_cwd("hello.txt", &cwd),
            "/fake/cwd/hello.txt"
        );
    }

    #[test]
    fn normalize_path_absolute() {
        assert_eq!(
            normalize_path_with_cwd("/src/main.rs", &PathBuf::from("/unused")),
            "/src/main.rs"
        );
    }

    #[test]
    fn normalize_path_nested() {
        let cwd = PathBuf::from("/fake/cwd");
        assert_eq!(
            normalize_path_with_cwd("src/lib.rs", &cwd),
            "/fake/cwd/src/lib.rs"
        );
    }

    #[test]
    fn normalize_path_dot_slash() {
        let cwd = PathBuf::from("/fake/cwd");
        assert_eq!(
            normalize_path_with_cwd("./hello.txt", &cwd),
            "/fake/cwd/hello.txt"
        );
    }

    #[test]
    fn normalize_path_dot_slash_nested() {
        let cwd = PathBuf::from("/fake/cwd");
        assert_eq!(
            normalize_path_with_cwd("./src/main.rs", &cwd),
            "/fake/cwd/src/main.rs"
        );
    }
}
