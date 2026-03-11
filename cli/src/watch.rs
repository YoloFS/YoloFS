// agfs CLI — watch.rs
//
// `agfs watch` — daemon mode: poll .agfs/mnt for ask requests,
// prompt the user (or apply policy), and write decisions back via ioctl.

use crate::ctl::{self, perm_from_str, perm_to_str, AgfsCtlRequest};
use anyhow::{Context, Result};
use colored::Colorize;
use std::io::{self, BufRead, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn prompt_decision(req: &AgfsCtlRequest) -> u8 {
    eprintln!(
        "\n{} pid={} comm={} op={} path={}",
        "[ask]".yellow().bold(),
        req.pid,
        req.comm_str(),
        req.op_str(),
        req.path_str(),
    );
    eprint!("  decision (allow/allow-rw/allow-ro/allow-rx/deny) [deny]: ");
    io::stderr().flush().ok();

    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).is_ok() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return ctl::AGFS_PERM_DENY;
        }
        perm_from_str(trimmed).unwrap_or(ctl::AGFS_PERM_DENY)
    } else {
        ctl::AGFS_PERM_DENY
    }
}

/// Interactive watch — prompts user for each request.
pub fn run() -> Result<()> {
    let agfs = ctl::agfs_dir()?;
    if !agfs.exists() {
        anyhow::bail!("no agfs session found (no .agfs/ directory)");
    }

    let ctl_file = ctl::open_ctl(&agfs)?;
    eprintln!(
        "{}",
        "agfs: watching for permission requests (Ctrl-C to stop)".cyan()
    );

    loop {
        let fd = ctl_file.as_raw_fd();
        let mut pollfd = nix::poll::PollFd::new(
            unsafe { std::os::unix::io::BorrowedFd::borrow_raw(fd) },
            nix::poll::PollFlags::POLLIN,
        );

        match nix::poll::poll(std::slice::from_mut(&mut pollfd), nix::poll::PollTimeout::NONE) {
            Ok(0) => continue,
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => return Err(e.into()),
        }

        let req = match ctl::ctl_read_request(&ctl_file) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("agfs watch: read error: {e}");
                continue;
            }
        };

        let decision = prompt_decision(&req);

        if let Err(e) = ctl::ctl_write_response(&ctl_file, req.id, decision) {
            eprintln!("agfs watch: write error: {e}");
        } else {
            eprintln!(
                "  → {} (req #{})",
                perm_to_str(decision),
                req.id
            );
        }
    }
}

/// Handle for stopping and joining the background watch thread.
pub struct WatchHandle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl WatchHandle {
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

/// Background watch — prompts for all ask requests.
/// Returns a handle to stop and join the thread.
pub fn run_background() -> Result<WatchHandle> {
    let agfs = ctl::agfs_dir()?;
    let ctl_file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_DIRECTORY)
        .open(agfs.join("mnt"))
        .context("opening .agfs/mnt for background watch")?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();

    let thread = std::thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            let fd = ctl_file.as_raw_fd();
            let mut pollfd = nix::poll::PollFd::new(
                unsafe { std::os::unix::io::BorrowedFd::borrow_raw(fd) },
                nix::poll::PollFlags::POLLIN,
            );

            match nix::poll::poll(
                std::slice::from_mut(&mut pollfd),
                nix::poll::PollTimeout::from(500u16),
            ) {
                Ok(0) => continue,
                Ok(_) => {}
                Err(_) => continue,
            }

            let req = match ctl::ctl_read_request(&ctl_file) {
                Ok(r) => r,
                Err(_) => continue,
            };

            let decision = prompt_decision(&req);
            let _ = ctl::ctl_write_response(&ctl_file, req.id, decision);
        }
    });

    Ok(WatchHandle { stop, thread: Some(thread) })
}
