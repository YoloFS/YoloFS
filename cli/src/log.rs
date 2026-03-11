// agfs CLI — log.rs
//
// `agfs log [--follow] [--dump]` — read and display binary log entries.

use crate::ctl::{self, AgfsLogEntry};
use anyhow::Result;
use colored::Colorize;
use std::io::Read;
use std::os::unix::io::AsRawFd;

fn print_entry(entry: &AgfsLogEntry) {
    let ts_secs = entry.timestamp_ns / 1_000_000_000;
    let ts_ms = (entry.timestamp_ns % 1_000_000_000) / 1_000_000;
    println!(
        "[{ts_secs}.{ts_ms:03}] {event:<10} pid={pid:<6} comm={comm:<16} perm={perm:<10} path={path} req={req}",
        event = entry.event_str(),
        pid = entry.pid,
        comm = entry.comm_str(),
        perm = entry.perm_str(),
        path = entry.path_str(),
        req = entry.req_id,
    );
}

pub fn run(follow: bool, dump: bool) -> Result<()> {
    let agfs = ctl::agfs_dir()?;
    if !agfs.exists() {
        anyhow::bail!("no agfs session found (no .agfs/ directory)");
    }

    let mut log_file = ctl::open_log(&agfs)?;

    if dump {
        // Non-blocking read: drain all available entries
        // Set O_NONBLOCK
        let fd = log_file.as_raw_fd();
        let flags = nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFL)?;
        let mut oflags = nix::fcntl::OFlag::from_bits_truncate(flags);
        oflags |= nix::fcntl::OFlag::O_NONBLOCK;
        nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFL(oflags))?;

        loop {
            let mut buf = [0u8; size_of::<AgfsLogEntry>()];
            match log_file.read_exact(&mut buf) {
                Ok(()) => {
                    let entry: AgfsLogEntry =
                        unsafe { std::ptr::read(buf.as_ptr() as *const AgfsLogEntry) };
                    print_entry(&entry);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.into()),
            }
        }
        return Ok(());
    }

    if follow {
        // Blocking read: continuously read and print entries
        eprintln!("{}", "agfs: tailing log (Ctrl-C to stop)".cyan());
    }

    loop {
        match ctl::log_read_entry(&mut log_file) {
            Ok(entry) => print_entry(&entry),
            Err(e) => {
                if !follow {
                    break;
                }
                // On error in follow mode, print and continue
                eprintln!("agfs log: {e}");
                break;
            }
        }

        if !follow {
            break;
        }
    }

    Ok(())
}
