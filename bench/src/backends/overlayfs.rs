use crate::backend::{self, Backend};
use crate::workload::{IterResult, Workload};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

pub struct Overlayfs;

impl Backend for Overlayfs {
    fn name(&self) -> &'static str {
        "overlayfs"
    }

    fn available(&self) -> bool {
        overlayfs_probe()
    }

    fn unavailable_reason(&self) -> Option<&'static str> {
        if !overlayfs_probe() {
            Some("overlayfs in user namespaces not supported (needs kernel >=5.11)")
        } else {
            None
        }
    }

    fn run_one(&self, workload: &dyn Workload, verbose: bool) -> Result<(IterResult, Vec<String>)> {
        let cache = dirs_next::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("agfs-bench");
        std::fs::create_dir_all(&cache)?;

        let root = tempfile::Builder::new()
            .prefix("agfs-bench-ovl-")
            .tempdir_in(&cache)
            .context("creating overlayfs session tempdir")?;

        let lower = root.path().join("lower");
        let upper = root.path().join("upper");
        let work = root.path().join("work");
        let merged = root.path().join("merged");
        for d in [&lower, &upper, &work, &merged] {
            std::fs::create_dir_all(d)?;
        }

        // Populate lower dir before mounting (not timed).
        let lower_work = lower.join(workload.work_dir());
        std::fs::create_dir_all(&lower_work)?;
        workload.populate_base(&lower_work)?;

        // Build the inner exec-workload command, then wrap it in unshare.
        // Inside the namespace, exec-overlayfs mounts the overlay then runs
        // the workload via the standard exec-workload protocol (READY marker).
        let self_exe = std::env::current_exe().context("resolving current executable")?;

        let mut cmd = Command::new("unshare");
        cmd.args(["--user", "--map-root-user", "--mount", "--"])
            .arg(&self_exe)
            .arg("exec-overlayfs")
            .arg("--name")
            .arg(workload.name())
            .arg("--lower")
            .arg(&lower)
            .arg("--upper")
            .arg(&upper)
            .arg("--work")
            .arg(&work)
            .arg("--merged")
            .arg(&merged);
        if verbose {
            cmd.arg("--verbose");
        }
        cmd.stderr(if verbose {
            Stdio::inherit()
        } else {
            Stdio::piped()
        });

        let result = backend::run_workload_subprocess(&mut cmd)?;

        // Commit: replay upper layer onto lower.
        // Regular files/dirs are copied; overlayfs whiteouts (char 0,0)
        // are applied as deletions in lower.
        let t_commit = Instant::now();
        commit_upper_to_lower(&upper, &lower)?;
        let commit_ms = t_commit.elapsed().as_millis() as u64;

        let total_ms = result.startup_ms + result.staging_ms + commit_ms;

        Ok((
            IterResult {
                init_ms: Some(result.startup_ms),
                staging_ms: Some(result.staging_ms),
                commit_ms: Some(commit_ms),
                total_ms,
            },
            vec![],
        ))
    }
}

/// Replay the overlayfs upper dir onto the lower dir.
///
/// - Regular files/dirs/symlinks: copied (cp -a semantics).
/// - Character device with major=0, minor=0: overlayfs whiteout → delete
///   the corresponding path in lower.
/// - Opaque dirs (trusted.overlay.opaque xattr): the lower dir is cleared
///   and replaced with the upper dir contents. We approximate this by
///   removing and re-creating the lower dir.
fn commit_upper_to_lower(upper: &std::path::Path, lower: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::FileTypeExt;

    for entry in std::fs::read_dir(upper).context("reading upper dir")? {
        let entry = entry?;
        let name = entry.file_name();
        let upper_path = entry.path();
        let lower_path = lower.join(&name);
        let ft = entry.file_type()?;

        if ft.is_char_device() {
            // Whiteout: delete from lower.
            if lower_path.is_dir() {
                let _ = std::fs::remove_dir_all(&lower_path);
            } else {
                let _ = std::fs::remove_file(&lower_path);
            }
        } else if ft.is_symlink() {
            // Recreate symlink.
            let target = std::fs::read_link(&upper_path)?;
            remove_any(&lower_path);
            std::os::unix::fs::symlink(&target, &lower_path)
                .with_context(|| format!("symlinking {}", lower_path.display()))?;
        } else if ft.is_dir() {
            // Opaque dir: overlayfs marks dirs with user.overlay.opaque when
            // they were rm'd and re-created. Wipe the lower dir first.
            if is_opaque_dir(&upper_path) {
                let _ = std::fs::remove_dir_all(&lower_path);
            }
            std::fs::create_dir_all(&lower_path)?;
            commit_upper_to_lower(&upper_path, &lower_path)?;
        } else {
            // Regular file: mv from upper to lower (O(1) rename on same fs).
            remove_any(&lower_path);
            std::fs::rename(&upper_path, &lower_path)
                .or_else(|_| {
                    // Cross-device fallback.
                    std::fs::copy(&upper_path, &lower_path)?;
                    std::fs::remove_file(&upper_path)?;
                    Ok::<_, std::io::Error>(())
                })
                .with_context(|| format!("moving {}", upper_path.display()))?;
        }
    }
    Ok(())
}

/// Remove a path regardless of type (file, dir, symlink).
fn remove_any(path: &std::path::Path) {
    if path.is_dir() && !path.is_symlink() {
        let _ = std::fs::remove_dir_all(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}

/// Check if an upper dir has the overlayfs opaque xattr, meaning it replaced
/// (not merged with) the lower dir.
fn is_opaque_dir(path: &std::path::Path) -> bool {
    // In user namespaces, overlayfs uses "user.overlay.opaque" (userxattr mount
    // option) instead of "trusted.overlay.opaque".
    let mut buf = [0u8; 2];
    for attr in ["user.overlay.opaque", "trusted.overlay.opaque"] {
        let c_path = match std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let c_attr = match std::ffi::CString::new(attr) {
            Ok(a) => a,
            Err(_) => continue,
        };
        let n = unsafe {
            libc::getxattr(
                c_path.as_ptr(),
                c_attr.as_ptr(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        };
        if n == 1 && buf[0] == b'y' {
            return true;
        }
    }
    false
}

/// Probe whether overlayfs works inside a user namespace.
fn overlayfs_probe() -> bool {
    let dir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(_) => return false,
    };
    for sub in ["lower", "upper", "work", "merged"] {
        if std::fs::create_dir(dir.path().join(sub)).is_err() {
            return false;
        }
    }
    let opts = format!(
        "lowerdir={lower},upperdir={upper},workdir={work}",
        lower = dir.path().join("lower").display(),
        upper = dir.path().join("upper").display(),
        work = dir.path().join("work").display(),
    );
    Command::new("unshare")
        .args([
            "--user",
            "--map-root-user",
            "--mount",
            "--",
            "mount",
            "-t",
            "overlay",
            "overlay",
            "-o",
        ])
        .arg(&opts)
        .arg(dir.path().join("merged"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
