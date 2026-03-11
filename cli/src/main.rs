// agfs CLI — main.rs

use agfs::{abort, commit, diff, log, mount, rule, run, status, watch};
use clap::{Parser, Subcommand};
use std::io::{self, BufRead, Write};

#[derive(Parser)]
#[command(name = "agfs", about = "Agentic filesystem — staging-commit + permission gating")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Command to run inside the sandbox (after --)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    exec_args: Vec<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Create .agfs/ layout and mount the filesystem
    Mount,
    /// Run a command inside the sandbox (requires existing mount)
    Run {
        /// Command to run (after --)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        exec_args: Vec<String>,
    },
    /// Show staged changes
    Status,
    /// Git-style diff of staged vs base
    Diff,
    /// Apply staged changes to base
    Commit,
    /// Discard staged changes
    Abort,
    /// Manage permission rules
    Rule {
        #[command(subcommand)]
        action: RuleAction,
    },
    /// Tail the debug log
    Log {
        /// Follow mode (like tail -f)
        #[arg(long)]
        follow: bool,
        /// Dump all buffered entries and exit
        #[arg(long)]
        dump: bool,
    },
    /// Handle ask requests (daemon mode)
    Watch,
}

#[derive(Subcommand)]
enum RuleAction {
    /// Add a permission rule
    Add {
        /// Path (relative to session root or absolute)
        path: String,
        /// Permission: allow, allow-rw, allow-ro, allow-rx, deny
        perm: String,
    },
    /// Remove a permission rule
    Remove {
        /// Path to remove rule from
        path: String,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Mount) => mount::run(),
        Some(Command::Run { exec_args }) => run::exec(&exec_args),
        Some(Command::Status) => status::run(),
        Some(Command::Diff) => diff::run(),
        Some(Command::Commit) => commit::run(),
        Some(Command::Abort) => abort::run(),
        Some(Command::Rule { action }) => match action {
            RuleAction::Add { path, perm } => rule::add(&path, &perm),
            RuleAction::Remove { path } => rule::remove(&path),
        },
        Some(Command::Log { follow, dump }) => log::run(follow, dump),
        Some(Command::Watch) => watch::run(),
        None => interactive_workflow(&cli.exec_args),
    }
}

/// Full workflow: mount → run → diff → ask commit/abort → unmount.
fn interactive_workflow(exec_args: &[String]) -> anyhow::Result<()> {
    // 1. Mount
    mount::run()?;

    // 2. Run (spawn + wait, not exec)
    let exit_status = run::spawn_and_wait(exec_args)?;

    if !exit_status.success() {
        eprintln!(
            "agfs: command exited with {}",
            exit_status.code().unwrap_or(-1)
        );
    }

    // 3. Show diff
    eprintln!("\n--- staged changes ---");
    diff::run()?;

    // 4. Ask commit or abort
    eprint!("\nagfs: commit these changes? [y/n] ");
    io::stderr().flush().ok();

    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;

    if line.trim().eq_ignore_ascii_case("y") {
        commit::run()?;
    } else {
        abort::run()?;
    }

    Ok(())
}
