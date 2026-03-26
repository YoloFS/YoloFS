use crate::backend::{self, Backend, CheckpointController, CheckpointOutcome};
use crate::workload::{CacheMode, IterResult, Workload};
use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

pub struct Overlayfs;

struct OverlayCheckpointController {
    root: PathBuf,
    lower: PathBuf,
    merged: PathBuf,
    work_dir: String,
    layer_idx: usize,
}

impl CheckpointController for OverlayCheckpointController {
    fn checkpoint(&mut self, _step: usize) -> Result<CheckpointOutcome> {
        let next = self.layer_idx + 1;

        let t = Instant::now();
        let next_upper = self.root.join(format!("upper-{next}"));
        let next_work = self.root.join(format!("work-{next}"));
        std::fs::create_dir_all(&next_upper)?;
        std::fs::create_dir_all(&next_work)?;

        sudo_umount(&self.merged)
            .with_context(|| format!("overlayfs checkpoint unmount at layer {next}"))?;

        let completed_layers = (0..=self.layer_idx)
            .map(|i| self.root.join(format!("upper-{i}")))
            .collect::<Vec<_>>();
        match sudo_mount_overlay_layers(
            &self.lower,
            &completed_layers,
            &next_upper,
            &next_work,
            &self.merged,
        ) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("  overlayfs: mount failed at layer {next}, stopping: {e:#}");
                return Ok(CheckpointOutcome::Stop);
            }
        }

        self.layer_idx = next;
        // After unmount+remount, the subprocess's cwd is stale. Redirect.
        let new_dest = self.merged.join(&self.work_dir);
        Ok(CheckpointOutcome::Continue {
            checkpoint_ms: t.elapsed().as_millis() as u64,
            next_dest: Some(new_dest),
        })
    }
}

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
        let upper0 = root.path().join("upper-0");
        let work0 = root.path().join("work-0");
        let merged = root.path().join("merged");
        for d in [&lower, &upper0, &work0, &merged] {
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
        let mut completed_layers: Vec<PathBuf> = Vec::new();
        let mut current_upper = upper0;
        let mut current_work = work0;
        sudo_mount_overlay_layers(
            &lower,
            &completed_layers,
            &current_upper,
            &current_work,
            &merged,
        )?;
        let init_ms = t_init.elapsed().as_millis() as u64;

        let dest = merged.join(workload.work_dir());
        std::fs::create_dir_all(&dest)?;

        if needs_prepare {
            workload.prepare_workdir(&dest)?;
        }
        if needs_chkpt {
            // Checkpoint: unmount and remount with a new upper/work pair while
            // turning the previous upper into an additional lower layer.
            sudo_umount(&merged).context("unmount for initial checkpoint")?;
            completed_layers.push(current_upper.clone());

            let next_idx = completed_layers.len();
            current_upper = root.path().join(format!("upper-{next_idx}"));
            current_work = root.path().join(format!("work-{next_idx}"));
            std::fs::create_dir_all(&current_upper)?;
            std::fs::create_dir_all(&current_work)?;

            sudo_mount_overlay_layers(
                &lower,
                &completed_layers,
                &current_upper,
                &current_work,
                &merged,
            )?;
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
            sudo_umount(&merged).context("unmount for cold cache flush")?;
            crate::workloads::drop_page_cache()?;
            sudo_mount_overlay_layers(
                &lower,
                &completed_layers,
                &current_upper,
                &current_work,
                &merged,
            )?;
            let mut cp = OverlayCheckpointController {
                root: root.path().to_path_buf(),
                lower: lower.clone(),
                merged: merged.clone(),
                work_dir: workload.work_dir().to_string(),
                layer_idx: completed_layers.len(),
            };
            let r = sp.go_with_checkpoint(&mut cp)?;
            current_upper = root.path().join(format!("upper-{}", cp.layer_idx));
            r
        } else {
            let sp = backend::spawn_and_await_ready(&mut cmd, false)?;
            let mut cp = OverlayCheckpointController {
                root: root.path().to_path_buf(),
                lower: lower.clone(),
                merged: merged.clone(),
                work_dir: workload.work_dir().to_string(),
                layer_idx: completed_layers.len(),
            };
            let r = sp.go_with_checkpoint(&mut cp)?;
            current_upper = root.path().join(format!("upper-{}", cp.layer_idx));
            r
        };

        // Measure status time: walk upper dir and classify changes,
        // printing each one to emulate agfs status output overhead.
        let status_ms = {
            let t = Instant::now();
            let mut sink = std::io::sink();
            if current_upper.exists() {
                report_upper_changes(&current_upper, &lower, "", &mut sink);
            }
            t.elapsed().as_millis() as u64
        };

        // Unmount before commit.
        sudo_umount_best_effort(&merged);

        // Commit: replay staged layers oldest→newest into lower.
        let t_commit = Instant::now();
        let final_idx = current_upper
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix("upper-"))
            .and_then(|s| s.parse::<usize>().ok())
            .context("deriving final overlay layer index")?;
        completed_layers = (0..final_idx)
            .map(|i| root.path().join(format!("upper-{i}")))
            .collect();
        current_upper = root.path().join(format!("upper-{final_idx}"));
        for layer in &completed_layers {
            commit_upper_to_lower(layer, &lower)?;
        }
        commit_upper_to_lower(&current_upper, &lower)?;
        let commit_ms = t_commit.elapsed().as_millis() as u64;

        let total_ms = init_ms + result.staging_ms + commit_ms;

        Ok((
            IterResult {
                init_ms: Some(init_ms),
                staging_ms: Some(result.staging_ms),
                status_ms: Some(status_ms),
                commit_ms: Some(commit_ms),
                total_ms,
                op_result: result.op_result,
                checkpoint_series: result.checkpoint_series,
            },
            vec![],
        ))
    }
}

fn sudo_mount_overlay_layers(
    lower_base: &std::path::Path,
    completed_layers: &[PathBuf],
    upper: &std::path::Path,
    work: &std::path::Path,
    merged: &std::path::Path,
) -> Result<()> {
    let mut lowers = completed_layers
        .iter()
        .rev()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>();
    lowers.push(lower_base.display().to_string());

    let opts = format!(
        "lowerdir={},upperdir={},workdir={}",
        lowers.join(":"),
        upper.display(),
        work.display()
    );
    let out = Command::new("sudo")
        .args(["-n", "mount", "-t", "overlay", "overlay", "-o"])
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

fn sudo_umount(merged: &std::path::Path) -> Result<()> {
    let out = Command::new("sudo")
        .args(["-n", "umount"])
        .arg(merged)
        .output()
        .context("unmount overlayfs")?;
    if !out.status.success() {
        bail!(
            "umount overlayfs failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

/// Best-effort unmount for cleanup paths (ignores errors).
fn sudo_umount_best_effort(merged: &std::path::Path) {
    let _ = Command::new("sudo")
        .args(["-n", "umount"])
        .arg(merged)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Walk the upper dir and classify each entry as added, modified, or deleted
/// by checking against the lower dir. Returns total change count.
/// Walk the upper dir, classify each entry (added/modified/deleted) by checking
/// against the lower dir, and print each change — emulating the output overhead
/// of `agfs status` for a fair timing comparison.
fn report_upper_changes(
    upper: &std::path::Path,
    lower: &std::path::Path,
    prefix: &str,
    sink: &mut dyn std::io::Write,
) -> usize {
    use std::os::unix::fs::FileTypeExt;
    let mut count = 0;
    let entries = match std::fs::read_dir(upper) {
        Ok(rd) => rd,
        Err(_) => return 0,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let upper_path = entry.path();
        let lower_path = lower.join(&name);
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        let rel = if prefix.is_empty() {
            name_str.to_string()
        } else {
            format!("{prefix}/{name_str}")
        };
        if ft.is_char_device() {
            let _ = writeln!(sink, "  deleted  {rel}");
            count += 1;
        } else if ft.is_dir() {
            count += report_upper_changes(&upper_path, &lower_path, &rel, sink);
        } else if lower_path.exists() {
            let _ = writeln!(sink, "  modified {rel}");
            count += 1;
        } else {
            let _ = writeln!(sink, "  added    {rel}");
            count += 1;
        }
    }
    count
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
            // rename() atomically replaces the destination if it exists
            // (for regular files). Only need remove_any if the destination
            // is a different type (e.g. dir being replaced by file).
            if lower_path.exists() && lower_path.is_dir() {
                remove_any(&lower_path);
            }
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
