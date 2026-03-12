// agfs CLI — watch.rs
//
// `agfs watch` — daemon mode: read ask requests via blocking ioctl,
// prompt the user, and write decisions back.

use crate::ioctl::{self, perm_from_str, perm_to_str, AgfsCtlRequest};
use anyhow::Result;
use colored::Colorize;
use std::io::{self, BufRead, Write};

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
            return ioctl::AGFS_PERM_DENY;
        }
        perm_from_str(trimmed).unwrap_or(ioctl::AGFS_PERM_DENY)
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

    loop {
        let req = match ioctl::read_request(&ctl_file) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("agfs watch: read error: {e}");
                continue;
            }
        };

        let decision = prompt_decision(&req);

        if let Err(e) = ioctl::write_response(&ctl_file, req.id, decision) {
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
