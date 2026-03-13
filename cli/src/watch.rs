// agfs CLI — watch.rs
//
// `agfs watch` — daemon mode: read ask requests via blocking ioctl,
// prompt the user, and write decisions back.
//
// `run_background()` — spawns a watch thread for the default workflow.
//
// When running as a background thread alongside `exec`, the child shell
// owns the terminal foreground group.  Before each prompt we temporarily
// claim the terminal (tcsetpgrp) so our stdin read succeeds, then hand
// it back to the previous foreground group.

use crate::ioctl::{self, perm_to_str, AgfsCtlRequest};
use anyhow::{Context, Result};
use colored::Colorize;
use nix::sys::signal::{signal, SigHandler, Signal};
use nix::unistd::{getpgrp, tcgetpgrp, tcsetpgrp, Pid};
use std::io::{self, BufRead, Write};
use std::os::unix::fs::OpenOptionsExt;

/// RAII guard that restores the terminal foreground group on drop.
struct TtyGuard(Option<Pid>);

impl Drop for TtyGuard {
    fn drop(&mut self) {
        release_tty(self.0);
    }
}

/// Temporarily make our process group the terminal foreground group so we
/// can read from stdin.  Returns a guard that restores the previous
/// foreground group when dropped.
fn claim_tty() -> TtyGuard {
    let stdin = io::stdin();
    let saved = tcgetpgrp(&stdin).ok();

    // Ignore SIGTTIN/SIGTTOU while we call tcsetpgrp — we may be a
    // background group right now.
    unsafe {
        signal(Signal::SIGTTIN, SigHandler::SigIgn).ok();
        signal(Signal::SIGTTOU, SigHandler::SigIgn).ok();
    }
    let pgid = getpgrp();
    tcsetpgrp(&stdin, pgid).ok();
    // Restore default handlers — we are now the foreground group so they
    // won't fire.
    unsafe {
        signal(Signal::SIGTTIN, SigHandler::SigDfl).ok();
        signal(Signal::SIGTTOU, SigHandler::SigDfl).ok();
    }
    TtyGuard(saved)
}

/// Give the terminal back to `prev` (the process group that owned it
/// before `claim_tty`).
fn release_tty(prev: Option<Pid>) {
    let Some(prev) = prev else { return };
    if prev == getpgrp() {
        return;
    }
    let stdin = io::stdin();
    unsafe {
        signal(Signal::SIGTTOU, SigHandler::SigIgn).ok();
    }
    tcsetpgrp(&stdin, prev).ok();
    unsafe {
        signal(Signal::SIGTTOU, SigHandler::SigDfl).ok();
    }
}

fn prompt_decision(req: &AgfsCtlRequest) -> u8 {
    let _guard = claim_tty();

    eprintln!(
        "{} {} {} {}",
        "[ask]".yellow().bold(),
        req.op_str(),
        req.path_str(),
        format!("(pid={} {})", req.pid, req.comm_str()).dimmed(),
    );
    eprint!(
        "  [{}]llow allow-[{}] allow-[{}] allow-[{}] [{}]eny (enter = deny): ",
        "a".blue().bold(),
        "rw".blue().bold(),
        "ro".blue().bold(),
        "rx".blue().bold(),
        "d".blue().bold(),
    );
    io::stderr().flush().ok();

    let mut line = String::new();
    let decision = if io::stdin().lock().read_line(&mut line).is_ok() {
        let trimmed = line.trim();
        match trimmed {
            "" | "d" | "deny" => ioctl::AGFS_PERM_DENY,
            "a" | "allow" => ioctl::AGFS_PERM_ALLOW,
            "rw" | "allow-rw" => ioctl::AGFS_PERM_ALLOW_RW,
            "ro" | "allow-ro" => ioctl::AGFS_PERM_ALLOW_RO,
            "rx" | "allow-rx" => ioctl::AGFS_PERM_ALLOW_RX,
            _ => {
                eprintln!("  unknown: {trimmed}, denying");
                ioctl::AGFS_PERM_DENY
            }
        }
    } else {
        ioctl::AGFS_PERM_DENY
    };

    // TTY is released automatically when _guard is dropped.
    decision
}

/// Interactive watch — blocks on ioctl read for each ask request.
pub fn run() -> Result<()> {
    let agfs = crate::session_dir()?;

    let ctl_file = ioctl::open(&agfs)?;
    eprintln!(
        "{}",
        "agfs: watching for permission requests (Ctrl-C to stop)".cyan()
    );

    watch_loop(&ctl_file);
    Ok(())
}

fn watch_loop(ctl_file: &std::fs::File) {
    loop {
        let req = match ioctl::read_request(ctl_file) {
            Ok(r) => r,
            Err(_) => break,
        };

        let decision = prompt_decision(&req);

        if let Err(e) = ioctl::write_response(ctl_file, req.id, decision) {
            eprintln!("agfs watch: write error: {e}");
        } else {
            // claim_tty/release_tty is not needed here because TOSTOP
            // is normally unset, so background stderr writes succeed.
            eprintln!(
                "  → {} (req #{})",
                perm_to_str(decision),
                req.id
            );
        }
    }
}

/// Spawn a background watch daemon thread that prompts for ask requests.
/// The thread runs until the process exits (it cannot be stopped early
/// because it blocks on a kernel ioctl).
pub fn run_background() -> Result<()> {
    let agfs = crate::session_dir()?;
    let ctl_file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(agfs.join("mnt"))
        .context("opening .agfs/mnt for background watch")?;

    std::thread::spawn(move || {
        watch_loop(&ctl_file);
    });

    Ok(())
}
