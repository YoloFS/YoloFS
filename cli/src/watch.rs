// agfs CLI — watch.rs
//
// `agfs watch` — daemon mode: read ask requests via blocking ioctl,
// prompt the user, and write decisions back.
//
// `run_background()` — spawns a watch thread for the default workflow.

use crate::ioctl::{self, perm_to_str, AgfsCtlRequest};
use anyhow::{Context, Result};
use colored::Colorize;
use std::io::{self, BufRead, Write};
use std::os::unix::fs::OpenOptionsExt;

fn prompt_decision(req: &AgfsCtlRequest) -> u8 {
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
    if io::stdin().lock().read_line(&mut line).is_ok() {
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
    }
}

/// Interactive watch — blocks on ioctl read for each ask request.
pub fn run() -> Result<()> {
    let agfs = crate::session_dir()?;
    if !agfs.exists() {
        anyhow::bail!("no agfs session found (no .agfs/ directory)");
    }

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
