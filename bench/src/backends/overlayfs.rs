use crate::backend::{self, Backend};
use crate::workload::{CacheMode, IterResult, Workload};
use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

pub struct Overlayfs;

impl Backend for Overlayfs {
    fn name(&self) -> &'static str {
        "overlayfs"
    }

    fn available(&self) -> bool {
        // Check that sudo mount -t overlay works.
        overlayfs_probe()
    }

    fn unavailable_reason(&self) -> Option<&'static str> {
        if !overlayfs_probe() {
            Some("overlayfs mount via sudo failed")
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

        let needs_prepare = workload.needs_prepare_workdir();
        let needs_chkpt = workload.needs_checkpoint();

        // Mount overlay with sudo (no user namespace).
        let t_init = Instant::now();
        sudo_mount_overlay(&lower, &upper, &work, &merged)?;
        let init_ms = t_init.elapsed().as_millis() as u64;

        let dest = merged.join(workload.work_dir());
        std::fs::create_dir_all(&dest)?;

        if needs_prepare {
            workload.prepare_workdir(&dest)?;
        }
        if needs_chkpt {
            // Checkpoint: unmount, commit upper to lower (demoting files
            // to a lower layer), remount with fresh upper.
            sudo_umount(&merged);
            commit_upper_to_lower(&upper, &lower)?;
            // Fresh upper/work for the new mount.
            std::fs::remove_dir_all(&upper).ok();
            std::fs::remove_dir_all(&work).ok();
            std::fs::create_dir_all(&upper)?;
            std::fs::create_dir_all(&work)?;
            sudo_mount_overlay(&lower, &upper, &work, &merged)?;
        }

        let cold = workload.cache_mode() == CacheMode::DropPageCache;
        let mut cmd =
            backend::exec_workload_cmd(workload.name(), std::path::Path::new("."), verbose, cold)?;
        cmd.current_dir(&dest);
        cmd.stderr(if verbose {
            Stdio::inherit()
        } else {
            Stdio::piped()
        });

        let result = if cold {
            // For cold: unmount overlay so kernel state is flushed, drop
            // caches, then remount. The subprocess waits for GO after READY.
            let sp = backend::spawn_and_await_ready(&mut cmd, true)?;
            sudo_umount(&merged);
            crate::workloads::drop_page_cache()?;
            sudo_mount_overlay(&lower, &upper, &work, &merged)?;
            sp.go()?
        } else {
            backend::run_workload_subprocess(&mut cmd, false)?
        };

        // Unmount before commit.
        sudo_umount(&merged);

        // Commit: replay upper layer onto lower.
        let t_commit = Instant::now();
        commit_upper_to_lower(&upper, &lower)?;
        let commit_ms = t_commit.elapsed().as_millis() as u64;

        let total_ms = init_ms + result.staging_ms + commit_ms;

        Ok((
            IterResult {
                init_ms: Some(init_ms),
                staging_ms: Some(result.staging_ms),
                commit_ms: Some(commit_ms),
                total_ms,
                op_result: result.op_result,
                checkpoint_series: result.checkpoint_series,
            },
            vec![],
        ))
    }
}

fn sudo_mount_overlay(
    lower: &std::path::Path,
    upper: &std::path::Path,
    work: &std::path::Path,
    merged: &std::path::Path,
) -> Result<()> {
    let opts = format!(
        "lowerdir={},upperdir={},workdir={}",
        lower.display(),
        upper.display(),
        work.display()
    );
    let out = Command::new("sudo")
        .args(["mount", "-t", "overlay", "overlay", "-o"])
        .arg(&opts)
        .arg(merged)
        .output()
        .context("mounting overlayfs with sudo")?;
    if !out.status.success() {
        bail!(
            "sudo mount overlay failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

fn sudo_umount(merged: &std::path::Path) {
    let _ = Command::new("sudo")
        .args(["umount"])
        .arg(merged)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Replay the overlayfs upper dir onto the lower dir.
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
            let target = std::fs::read_link(&upper_path)?;
            remove_any(&lower_path);
            std::os::unix::fs::symlink(&target, &lower_path)
                .with_context(|| format!("symlinking {}", lower_path.display()))?;
        } else if ft.is_dir() {
            if is_opaque_dir(&upper_path) {
                let _ = std::fs::remove_dir_all(&lower_path);
            }
            std::fs::create_dir_all(&lower_path)?;
            commit_upper_to_lower(&upper_path, &lower_path)?;
        } else {
            remove_any(&lower_path);
            std::fs::rename(&upper_path, &lower_path)
                .or_else(|_| {
                    std::fs::copy(&upper_path, &lower_path)?;
                    std::fs::remove_file(&upper_path)?;
                    Ok::<_, std::io::Error>(())
                })
                .with_context(|| format!("moving {}", upper_path.display()))?;
        }
    }
    Ok(())
}

fn remove_any(path: &std::path::Path) {
    if path.is_dir() && !path.is_symlink() {
        let _ = std::fs::remove_dir_all(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}

fn is_opaque_dir(path: &std::path::Path) -> bool {
    let mut buf = [0u8; 2];
    for attr in ["trusted.overlay.opaque", "user.overlay.opaque"] {
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

/// Probe whether overlayfs works with sudo mount.
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
    let merged = dir.path().join("merged");
    let ok = Command::new("sudo")
        .args(["mount", "-t", "overlay", "overlay", "-o"])
        .arg(&opts)
        .arg(&merged)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        let _ = Command::new("sudo")
            .args(["umount"])
            .arg(&merged)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    ok
}
