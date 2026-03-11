// agfs CLI — main.rs

use agfs::{abort, commit, diff, log, mount, rule, run, status, watch};
use clap::{Parser, Subcommand};
use colored::Colorize;
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
        Some(Command::Diff) => diff::run().map(|_| ()),
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

/// Full workflow: mount → run → diff → ask commit/abort/stage.
fn interactive_workflow(exec_args: &[String]) -> anyhow::Result<()> {
    // 1. Mount
    mount::run()?;

    // 2. Run (spawn + wait, not exec)
    let exit_status = run::spawn_and_wait(exec_args)?;

    if !exit_status.success() {
        eprintln!(
            "{} {}",
            "agfs: command exited with".red(),
            exit_status.code().unwrap_or(-1)
        );
    }

    // 3. Show diff
    eprintln!();
    let has_changes = diff::run()?;

    if !has_changes {
        abort::run()?;
        return Ok(());
    }

    // 4. Ask commit, abort, or stage
    eprint!(
        "\n{} ",
        "Choose [c]ommit, [a]bort, or [s]tage [default: stage]:".bold()
    );
    io::stderr().flush().ok();

    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;

    match line.trim().to_ascii_lowercase().as_str() {
        "c" | "commit" => commit::run()?,
        "a" | "abort" => abort::run()?,
        _ => {
            eprintln!(
                "{}",
                "agfs: changes kept staged — use `agfs commit` or `agfs abort` to finish".cyan()
            );
        }
    }

    Ok(())
}
