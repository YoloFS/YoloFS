// agfs CLI — watch.rs
//
// `agfs watch` — daemon mode: poll .agfs/ctl for ask requests,
// prompt the user (or apply policy), and write decisions back.

use crate::ctl::{self, perm_from_str, perm_to_str, AgfsCtlRequest};
use anyhow::Result;
use std::io::{self, BufRead, Write};
use std::os::unix::io::AsRawFd;

fn prompt_decision(req: &AgfsCtlRequest) -> u8 {
    eprintln!(
        "\n\x1b[1;33m[ask]\x1b[0m pid={} comm={} op={} path={}",
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

pub fn run() -> Result<()> {
    let agfs = ctl::agfs_dir()?;
    if !agfs.exists() {
        anyhow::bail!("no agfs session found (no .agfs/ directory)");
    }

    let mut ctl_file = ctl::open_ctl(&agfs)?;
    eprintln!("agfs: watching for permission requests (Ctrl-C to stop)");

    loop {
        // Poll for requests
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

        // Read the request
        let req = match ctl::ctl_read_request(&mut ctl_file) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("agfs watch: read error: {e}");
                continue;
            }
        };

        // Prompt user for decision
        let decision = prompt_decision(&req);

        // Write the response
        if let Err(e) = ctl::ctl_write_response(&mut ctl_file, req.id, decision) {
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
