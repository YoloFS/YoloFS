// agfs CLI — main.rs

use agfs::{abort, commit, config, diff, exec, init, mount, snapshot, status, watch};
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::io::{self, BufRead, Write};

#[derive(Parser)]
#[command(
    name = "agfs",
    about = "Agentic filesystem — staging-commit + permission gating"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Snapshot before running the command
    #[arg(long)]
    snapshot: bool,

    /// Command to run inside the sandbox (after --)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    exec_args: Vec<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Create agfs.toml and load the kernel module
    Init,
    /// Unload then reload the kernel module
    Reinit,
    /// Unload the kernel module
    Deinit,
    /// Create .agfs/ layout and mount the filesystem
    Mount,
    /// Unmount and clean up the session
    Unmount,
    /// Unmount then remount (picks up new agfs.toml mount options)
    Remount,
    /// Execute a command inside the sandbox (requires existing mount)
    Exec {
        /// Snapshot before executing the command
        #[arg(long)]
        snapshot: bool,

        /// Command to run (after --)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        exec_args: Vec<String>,
    },
    /// Show staged changes
    Status {
        /// Show state at a named snapshot
        #[arg(long)]
        at: Option<String>,
    },
    /// Git-style diff of staged vs base
    Diff {
        /// Diff changes since a named snapshot
        #[arg(long)]
        from: Option<String>,
    },
    /// Apply staged changes to base
    Commit {
        /// Commit only changes up to a named snapshot
        #[arg(long)]
        at: Option<String>,
    },
    /// Discard staged changes
    Abort,
    /// Create or list snapshots
    Snapshot {
        #[command(subcommand)]
        action: Option<SnapshotAction>,

        /// Snapshot name (when not using a subcommand)
        #[arg(trailing_var_arg = true)]
        name: Vec<String>,
    },
    /// Manage permission rules
    Rule {
        #[command(subcommand)]
        action: RuleAction,
    },
    /// Handle ask requests (daemon mode)
    Watch {
        /// Automatically allow all requests without prompting
        #[arg(long)]
        allow_all: bool,
    },
}

#[derive(Subcommand)]
enum RuleAction {
    /// Add a permission rule
    Add {
        /// Path (relative to session root or absolute)
        path: String,
        /// Permission: allow, allow-rw, allow-ro, allow-rx, deny, ask
        perm: String,
    },
    /// Remove a permission rule
    Remove {
        /// Path to remove rule from
        path: String,
    },
}

#[derive(Subcommand)]
enum SnapshotAction {
    /// List all snapshots
    List,
}

fn main() -> ! {
    let code = match run_cli() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Error: {e:?}");
            1
        }
    };
    std::process::exit(code as i32);
}

fn run_cli() -> anyhow::Result<u8> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Exec {
            exec_args,
            snapshot,
        }) => return exec::run(&exec_args, snapshot),
        Some(Command::Init) => init::init()?,
        Some(Command::Reinit) => init::reinit()?,
        Some(Command::Deinit) => init::deinit()?,
        Some(Command::Mount) => mount::mount()?,
        Some(Command::Unmount) => mount::unmount()?,
        Some(Command::Remount) => mount::remount()?,
        Some(Command::Status { at }) => status::run(at.as_deref())?,
        Some(Command::Diff { from }) => {
            diff::run(from.as_deref())?;
        }
        Some(Command::Commit { at }) => commit::run(at.as_deref())?,
        Some(Command::Abort) => abort::run()?,
        Some(Command::Snapshot { action, name }) => match action {
            Some(SnapshotAction::List) => snapshot::list()?,
            None => {
                let snap_name = if name.is_empty() {
                    None
                } else {
                    Some(name.join(" "))
                };
                snapshot::create(snap_name.as_deref())?;
            }
        },
        Some(Command::Rule { action }) => match action {
            RuleAction::Add { path, perm } => config::add_rule(&path, &perm)?,
            RuleAction::Remove { path } => config::remove_rule(&path)?,
        },
        Some(Command::Watch { allow_all }) => watch::run(allow_all)?,
        None => {
            let has_separator = std::env::args().any(|a| a == "--");
            if !cli.exec_args.is_empty() && !has_separator {
                Cli::parse_from(["agfs", "--help"]);
                unreachable!();
            }
            return run(&cli.exec_args, cli.snapshot);
        }
    }

    Ok(0)
}

/// Full workflow: mount (if needed) → watch → exec → diff → commit/abort/stage.
fn run(exec_args: &[String], do_snapshot: bool) -> anyhow::Result<u8> {
    // 1. Mount if not already mounted
    mount::mount()?;

    // 2. Start background watch daemon (prompts for permission asks)
    watch::run_background()?;

    // 3. Exec (spawn + wait) — continue to diff even if command fails
    let cmd_exit_code = exec::run(exec_args, do_snapshot).unwrap_or(1);

    // 4. Show diff
    let has_changes = diff::run(None)?;

    if !has_changes {
        return Ok(cmd_exit_code);
    }

    // 5. Ask commit, abort, or stage
    eprint!(
        "\n{} ",
        "Choose [c]ommit, [a]bort, or [s]tage [default: stage]:".bold()
    );
    io::stderr().flush().ok();

    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;

    match line.trim().to_ascii_lowercase().as_str() {
        "c" | "commit" => commit::run(None)?,
        "a" | "abort" => abort::run()?,
        _ => {
            eprintln!(
                "{}",
                "agfs: changes kept staged — use `agfs commit` or `agfs abort` to finish".cyan()
            );
        }
    }

    Ok(cmd_exit_code)
}
