// yolo CLI — watch.rs
//
// `yolo watch` — daemon mode: read ask requests via blocking ioctl,
// prompt the user, and write decisions back.
//
// `run_background()` — spawns a watch thread for the default workflow.
//
// When running as a background thread alongside `exec`, the child shell
// owns the terminal foreground group.  Before each prompt we temporarily
// claim the terminal (tcsetpgrp) so our stdin read succeeds, then hand
// it back to the previous foreground group.

use crate::ioctl::{self, PermRequest};
use crate::perm::Perm;
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
        "{} {} wants to {} {}",
        "[ask]".yellow().bold(),
        req.comm_str(),
        req.op_str(),
        req.path_str(),
    );
    eprint!(
        "  [{}]llow [{}]rite-ask [{}]ead-only [{}]eny (enter = allow): ",
        "a".blue().bold(),
        "w".blue().bold(),
        "r".blue().bold(),
        "d".blue().bold(),
    );
    io::stderr().flush().ok();

    let mut line = String::new();
    // TTY is released automatically when _guard is dropped.
    if io::stdin().lock().read_line(&mut line).is_ok() {
        let trimmed = line.trim();
        parse_input(trimmed).unwrap_or_else(|| {
            eprintln!("  unknown: {trimmed}, denying");
            Perm::Deny
        })
    } else {
        Perm::Deny
    }
}

/// Interactive watch — blocks on ioctl read for each ask request.
pub fn run(allow_all: bool) -> Result<()> {
    let yolofs = crate::utils::session_dir()?;

    let ctl_file = ioctl::open(&yolofs)?;
    if allow_all {
        eprintln!(
            "{}",
            "yolo: watching for permission requests — allowing all (Ctrl-C to stop)".cyan()
        );
    } else {
        eprintln!(
            "{}",
            "yolo: watching for permission requests (Ctrl-C to stop)".cyan()
        );
    }

    watch_loop(&ctl_file, allow_all)
}

fn watch_loop(ctl_file: &std::fs::File, allow_all: bool) -> Result<()> {
    loop {
        let req = match ioctl::get_ask(ctl_file) {
            Ok(r) => r,
            Err(nix::errno::Errno::EBUSY) => {
                anyhow::bail!("another yolo watch is already running");
            }
            Err(_) => return Ok(()),
        };

        let decision = if allow_all {
            eprintln!(
                "{} {} wants to {} {}",
                "[ask]".yellow().bold(),
                req.comm_str(),
                req.op_str(),
                req.path_str(),
            );
            Perm::Allow
        } else {
            prompt_decision(&req)
        };

        if let Err(e) = ioctl::put_decision(ctl_file, req.id, decision.to_ioctl()) {
            eprintln!("yolo watch: write error: {e}");
        } else {
            // claim_tty/release_tty is not needed here because TOSTOP
            // is normally unset, so background stderr writes succeed.
            eprintln!("  → {} (req #{})", decision, req.id);
        }
    }
}

/// Parse user input (already trimmed) into an ask decision.
///
/// This is a pure function extracted from `prompt_decision` so it can be
/// unit-tested without terminal access.
fn parse_input(input: &str) -> Option<Perm> {
    match input {
        "" | "a" | "allow" => Some(Perm::Allow),
        "w" | "write-ask" | "writeask" => Some(Perm::WriteAsk),
        "r" | "read-only" | "readonly" => Some(Perm::ReadOnly),
        "d" | "deny" => Some(Perm::Deny),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_is_allow() {
        assert_eq!(parse_input(""), Some(Perm::Allow));
    }

    #[test]
    fn d_is_deny() {
        assert_eq!(parse_input("d"), Some(Perm::Deny));
    }

    #[test]
    fn deny_is_deny() {
        assert_eq!(parse_input("deny"), Some(Perm::Deny));
    }

    #[test]
    fn a_is_allow() {
        assert_eq!(parse_input("a"), Some(Perm::Allow));
    }

    #[test]
    fn allow_is_allow() {
        assert_eq!(parse_input("allow"), Some(Perm::Allow));
    }

    #[test]
    fn r_is_read_only() {
        assert_eq!(parse_input("r"), Some(Perm::ReadOnly));
    }

    #[test]
    fn read_only_is_read_only() {
        assert_eq!(parse_input("read-only"), Some(Perm::ReadOnly));
    }

    #[test]
    fn w_is_write_ask() {
        assert_eq!(parse_input("w"), Some(Perm::WriteAsk));
    }

    #[test]
    fn write_ask_is_write_ask() {
        assert_eq!(parse_input("write-ask"), Some(Perm::WriteAsk));
    }

    #[test]
    fn unknown_input_is_none() {
        assert_eq!(parse_input("xyz"), None);
    }

    #[test]
    fn rule_only_inputs_are_none() {
        assert_eq!(parse_input("ask"), None);
        assert_eq!(parse_input("hide"), None);
        assert_eq!(parse_input("h"), None);
    }

    #[test]
    fn whitespace_input_is_none() {
        // parse_input receives already-trimmed input, so whitespace is
        // treated as unknown.
        assert_eq!(parse_input("  a  "), None);
    }
}
