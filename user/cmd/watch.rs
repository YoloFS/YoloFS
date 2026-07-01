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

use crate::ioctl::{self, Ask};
use crate::perm::Decision;
use crate::report;
use anyhow::Result;
use nix::sys::signal::{SigHandler, Signal, signal};
use nix::unistd::{Pid, getpgrp, tcgetpgrp, tcsetpgrp};
use std::io::{self, BufRead};

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

fn print_ask(req: &Ask) {
    report::warn(format!(
        "{} wants to {} {}",
        req.comm_str(),
        req.op_str(),
        req.access_path_str(),
    ));
    let source = req.rule_path.as_deref().unwrap_or("default");
    report::detail(format!("rule: {source} {}", rule_phrase(req.rule_perm)));
}

fn rule_phrase(perm: crate::perm::Perm) -> &'static str {
    match perm {
        crate::perm::Perm::Ask => "asks",
        crate::perm::Perm::WriteAsk => "asks before writes",
        crate::perm::Perm::ReadOnly => "is read-only",
        crate::perm::Perm::Deny => "denies access",
        crate::perm::Perm::Allow => "allows access",
        crate::perm::Perm::Hide => "is hidden",
    }
}

fn prompt_decision(req: &Ask) -> Decision {
    let _guard = claim_tty();

    print_ask(req);
    report::prompt("allow [y]es / [d]eny (enter = yes):");

    let mut line = String::new();
    // TTY is released automatically when _guard is dropped.
    if io::stdin().lock().read_line(&mut line).is_ok() {
        let trimmed = line.trim();
        parse_input(trimmed).unwrap_or_else(|| {
            report::detail(format!("unknown: {trimmed}, denying"));
            Decision::Deny
        })
    } else {
        Decision::Deny
    }
}

/// Interactive watch — blocks on ioctl read for each ask request.
pub fn run(allow_all: bool) -> Result<()> {
    let yolofs = crate::utils::session_dir()?;

    let ctl_file = ioctl::open(&yolofs)?;

    // Claim the daemon slot before announcing readiness: until a daemon has
    // claimed, the kernel fast-denies asks, so an op racing our startup would be
    // wrongly denied. Claiming up front (a non-blocking ASK_PEEK) closes that.
    // A peek does not consume, so any ask that raced in stays queued for the
    // loop below to handle.
    ioctl::claim_daemon(&ctl_file)?;

    if allow_all {
        report::info("watching for permission requests — allowing all (Ctrl-C to stop)");
    } else {
        report::info("watching for permission requests (Ctrl-C to stop)");
    }

    watch_loop(&ctl_file, allow_all)
}

/// Decide one ask (auto-allow under --allow-all, else prompt) and write it back.
fn handle_ask(ctl_file: &std::fs::File, req: Ask, allow_all: bool) {
    let decision = if allow_all {
        print_ask(&req);
        Decision::Allow
    } else {
        prompt_decision(&req)
    };

    // claim_tty/release_tty is not needed here because TOSTOP is normally
    // unset, so background stderr writes succeed.
    match ioctl::ask_decide(ctl_file, req.id, decision) {
        Ok(()) => report::detail(format!("→ {} (req #{})", decision, req.id)),
        // The ask timed out (or its process was killed) before we answered —
        // the kernel already resolved it. Benign; not a write failure.
        Err(e) if e.downcast_ref::<nix::errno::Errno>() == Some(&nix::errno::Errno::ENOENT) => {
            report::detail(format!("→ {} (req #{}) — already resolved", decision, req.id));
        }
        Err(e) => report::warn(format!("write error: {e}")),
    }
}

fn watch_loop(ctl_file: &std::fs::File, allow_all: bool) -> Result<()> {
    loop {
        let req = match ioctl::ask_peek(ctl_file) {
            Ok(r) => r,
            Err(nix::errno::Errno::EBUSY) => {
                anyhow::bail!("another yolo watch is already running");
            }
            Err(_) => return Ok(()),
        };

        handle_ask(ctl_file, req, allow_all);
    }
}

/// Parse user input (already trimmed) into an ask decision.
///
/// This is a pure function extracted from `prompt_decision` so it can be
/// unit-tested without terminal access.
fn parse_input(input: &str) -> Option<Decision> {
    match input {
        "" | "y" | "yes" | "allow" => Some(Decision::Allow),
        "d" | "deny" => Some(Decision::Deny),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_is_allow() {
        assert_eq!(parse_input(""), Some(Decision::Allow));
    }

    #[test]
    fn d_is_deny() {
        assert_eq!(parse_input("d"), Some(Decision::Deny));
    }

    #[test]
    fn deny_is_deny() {
        assert_eq!(parse_input("deny"), Some(Decision::Deny));
    }

    #[test]
    fn y_is_allow() {
        assert_eq!(parse_input("y"), Some(Decision::Allow));
    }

    #[test]
    fn yes_is_allow() {
        assert_eq!(parse_input("yes"), Some(Decision::Allow));
    }

    #[test]
    fn allow_is_allow() {
        assert_eq!(parse_input("allow"), Some(Decision::Allow));
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
        assert_eq!(parse_input("a"), None);
        assert_eq!(parse_input("w"), None);
        assert_eq!(parse_input("write-ask"), None);
        assert_eq!(parse_input("r"), None);
        assert_eq!(parse_input("read-only"), None);
    }

    #[test]
    fn whitespace_input_is_none() {
        // parse_input receives already-trimmed input, so whitespace is
        // treated as unknown.
        assert_eq!(parse_input("  a  "), None);
    }
}
