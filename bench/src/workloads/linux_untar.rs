use crate::workload::{Workload, WorkloadKind};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use yolofs::config::Perm;

/// Compute a relative path from `from` (a directory) to `to`.
fn relative_path(from: &Path, to: &Path) -> PathBuf {
    // Canonicalize both to get absolute paths.
    let from = std::fs::canonicalize(from).unwrap_or_else(|_| from.to_path_buf());
    let to = std::fs::canonicalize(to).unwrap_or_else(|_| to.to_path_buf());

    let mut from_parts = from.components().peekable();
    let mut to_parts = to.components().peekable();

    // Skip common prefix.
    while let (Some(a), Some(b)) = (from_parts.peek(), to_parts.peek()) {
        if a != b {
            break;
        }
        from_parts.next();
        to_parts.next();
    }

    // Go up for remaining `from` components, then down for remaining `to` components.
    let mut rel = PathBuf::new();
    for _ in from_parts {
        rel.push("..");
    }
    for part in to_parts {
        rel.push(part);
    }
    rel
}

const LINUX_TARBALL_URL: &str = "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.12.1.tar.xz";
const LINUX_TARBALL_FILE: &str = "linux-6.12.1.tar.xz";

pub struct LinuxUntar {
    fixture_dir: PathBuf,
    tarball_path: PathBuf,
}

pub fn details() -> crate::workloads::WorkloadDetails {
    crate::workloads::workload_details(
        "Session macrobenchmark that untars a Linux source release into the mounted destination.",
        "Caches one Linux source tarball under ~/.cache/yolo-bench/linux-tar/ and reuses it across runs.",
        None,
        "Runs `tar -xJf <cached-tarball> -C <dest> --strip-components=1`.",
        file!(),
    )
}

impl LinuxUntar {
    pub fn new() -> Self {
        let fixture_dir = dirs_next::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("yolo-bench/linux-tar");
        let tarball_path = fixture_dir.join(LINUX_TARBALL_FILE);
        Self {
            fixture_dir,
            tarball_path,
        }
    }

    fn ensure_tarball(&self) -> Result<()> {
        if self.tarball_path.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.fixture_dir)
            .context("creating linux-tar fixture directory")?;
        eprintln!("Downloading Linux source tarball (one-time fixture setup)...");
        let status = Command::new("curl")
            .args(["-fL", "--retry", "3", "-o"])
            .arg(&self.tarball_path)
            .arg(LINUX_TARBALL_URL)
            .status()
            .context("running curl for Linux tarball")?;
        if !status.success() {
            bail!("failed to download Linux tarball fixture")
        }
        Ok(())
    }
}

impl Workload for LinuxUntar {
    fn name(&self) -> &'static str {
        "linux-untar"
    }

    fn kind(&self) -> WorkloadKind {
        WorkloadKind::Macro
    }

    fn description(&self) -> &'static str {
        "untar a cached Linux source tarball (~80k files)"
    }

    fn work_dir(&self) -> &'static str {
        "linux-untar-dest"
    }

    fn ensure_fixture(&self) -> Result<()> {
        self.ensure_tarball()
    }

    fn realistic_rules(&self, session_root: &Path) -> Vec<(String, Perm)> {
        vec![
            (session_root.to_string_lossy().into_owned(), Perm::AllowRw),
            (
                self.fixture_dir.to_string_lossy().into_owned(),
                Perm::AllowRo,
            ),
        ]
    }

    fn run(&self, dest: &Path, verbose: bool) -> Result<()> {
        std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
        std::env::set_current_dir(dest).with_context(|| format!("chdir to {}", dest.display()))?;

        // Use a relative path to the tarball so it resolves correctly
        // inside a YoloFS exec chroot.
        let tarball_rel = relative_path(dest, &self.tarball_path);

        let status = Command::new("tar")
            .arg("-xJf")
            .arg(&tarball_rel)
            .arg("--strip-components=1")
            .stdout(if verbose {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .stderr(if verbose {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .status()
            .with_context(|| format!("running tar extract from {}", tarball_rel.display()))?;
        if !status.success() {
            bail!("linux tar extraction failed")
        }
        Ok(())
    }
}
