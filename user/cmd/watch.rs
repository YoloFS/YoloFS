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

use crate::config::Perm;
use crate::ioctl::{self, PermRequest};
use anyhow::Result;
use colored::Colorize;
use nix::sys::signal::{SigHandler, Signal, signal};
use nix::unistd::{Pid, getpgrp, tcgetpgrp, tcsetpgrp};
use std::io::{self, BufRead, Write};

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

fn prompt_decision(req: &PermRequest) -> Perm {
    let _guard = claim_tty();

    eprintln!(
        "{} {} {} {}",
        "[ask]".yellow().bold(),
        req.op_str(),
        req.path_str(),
        format!("(pid={} {})", req.pid, req.comm_str()).dimmed(),
    );
    eprint!(
        "  [{}]llow allow-[{}] allow-[{}] allow-[{}] [{}]eny (enter = allow): ",
        "a".blue().bold(),
        "rw".blue().bold(),
        "ro".blue().bold(),
        "rx".blue().bold(),
        "d".blue().bold(),
    );
    io::stderr().flush().ok();

    let mut line = String::new();
    // TTY is released automatically when _guard is dropped.
    if io::stdin().lock().read_line(&mut line).is_ok() {
        let trimmed = line.trim();
        let perm = parse_input(trimmed);
        if matches!(perm, Perm::Deny) && !trimmed.is_empty() && trimmed != "d" && trimmed != "deny"
        {
            eprintln!("  unknown: {trimmed}, denying");
        }
        perm
    } else {
        Perm::Deny
    }
}

/// Interactive watch — blocks on ioctl read for each ask request.
pub fn run(allow_all: bool) -> Result<()> {
    let agfs = crate::utils::session_dir()?;

    let ctl_file = ioctl::open(&agfs)?;
    if allow_all {
        eprintln!(
            "{}",
            "agfs: watching for permission requests — allowing all (Ctrl-C to stop)".cyan()
        );
    } else {
        eprintln!(
            "{}",
            "agfs: watching for permission requests (Ctrl-C to stop)".cyan()
        );
    }

    watch_loop(&ctl_file, allow_all)
}

fn watch_loop(ctl_file: &std::fs::File, allow_all: bool) -> Result<()> {
    loop {
        let req = match ioctl::read_request(ctl_file) {
            Ok(r) => r,
            Err(nix::errno::Errno::EBUSY) => {
                anyhow::bail!("another agfs watch is already running");
            }
            Err(_) => return Ok(()),
        };

        let decision = if allow_all {
            eprintln!(
                "{} {} {} {}",
                "[ask]".yellow().bold(),
                req.op_str(),
                req.path_str(),
                format!("(pid={} {})", req.pid, req.comm_str()).dimmed(),
            );
            Perm::Allow
        } else {
            prompt_decision(&req)
        };

        if let Err(e) = ioctl::write_response(ctl_file, req.id, decision.to_ioctl()) {
            eprintln!("agfs watch: write error: {e}");
        } else {
            // claim_tty/release_tty is not needed here because TOSTOP
            // is normally unset, so background stderr writes succeed.
            eprintln!("  → {} (req #{})", decision, req.id);
        }
    }
}

/// Parse user input (already trimmed) into a [`Perm`] decision.
///
/// This is a pure function extracted from `prompt_decision` so it can be
/// unit-tested without terminal access.
fn parse_input(input: &str) -> Perm {
    match input {
        "" | "a" | "allow" => Perm::Allow,
        "d" | "deny" => Perm::Deny,
        "rw" | "allow-rw" => Perm::AllowRw,
        "ro" | "allow-ro" => Perm::AllowRo,
        "rx" | "allow-rx" => Perm::AllowRx,
        _ => Perm::Deny,
    }
}

/// Spawn a background watch daemon thread that prompts for ask requests.
/// The thread runs until the process exits (it cannot be stopped early
/// because it blocks on a kernel ioctl).
pub fn run_background() -> Result<()> {
    let agfs = crate::utils::session_dir()?;
    let ctl_file = ioctl::open(&agfs)?;

    std::thread::spawn(move || {
        if let Err(e) = watch_loop(&ctl_file, false) {
            eprintln!("{} {e}", "agfs watch:".red());
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_is_allow() {
        assert_eq!(parse_input(""), Perm::Allow);
    }

    #[test]
    fn d_is_deny() {
        assert_eq!(parse_input("d"), Perm::Deny);
    }

    #[test]
    fn deny_is_deny() {
        assert_eq!(parse_input("deny"), Perm::Deny);
    }

    #[test]
    fn a_is_allow() {
        assert_eq!(parse_input("a"), Perm::Allow);
    }

    #[test]
    fn allow_is_allow() {
        assert_eq!(parse_input("allow"), Perm::Allow);
    }

    #[test]
    fn rw_is_allow_rw() {
        assert_eq!(parse_input("rw"), Perm::AllowRw);
    }

    #[test]
    fn allow_rw_is_allow_rw() {
        assert_eq!(parse_input("allow-rw"), Perm::AllowRw);
    }

    #[test]
    fn ro_is_allow_ro() {
        assert_eq!(parse_input("ro"), Perm::AllowRo);
    }

    #[test]
    fn allow_ro_is_allow_ro() {
        assert_eq!(parse_input("allow-ro"), Perm::AllowRo);
    }

    #[test]
    fn rx_is_allow_rx() {
        assert_eq!(parse_input("rx"), Perm::AllowRx);
    }

    #[test]
    fn allow_rx_is_allow_rx() {
        assert_eq!(parse_input("allow-rx"), Perm::AllowRx);
    }

    #[test]
    fn unknown_input_is_deny() {
        assert_eq!(parse_input("xyz"), Perm::Deny);
    }

    #[test]
    fn whitespace_input_is_deny() {
        // parse_input receives already-trimmed input, so whitespace is
        // treated as unknown and maps to Deny.
        assert_eq!(parse_input("  a  "), Perm::Deny);
    }
}
