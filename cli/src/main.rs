// agfs CLI — main.rs

use agfs::{abort, commit, config, diff, exec, log, mount, status, unmount, watch};
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
    /// Create agfs.toml in the current directory
    Init,
    /// Create .agfs/ layout and mount the filesystem
    Mount,
    /// Unmount and clean up the session
    Unmount,
    /// Execute a command inside the sandbox (requires existing mount)
    Exec {
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
        /// Permission: allow, allow-rw, allow-ro, allow-rx, deny, ask
        perm: String,
    },
    /// Remove a permission rule
    Remove {
        /// Path to remove rule from
        path: String,
    },
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
        Some(Command::Exec { exec_args }) => return exec::run(&exec_args),
        Some(Command::Init) => config::init()?,
        Some(Command::Mount) => mount::run()?,
        Some(Command::Unmount) => unmount::run()?,
        Some(Command::Status) => status::run()?,
        Some(Command::Diff) => { diff::run()?; }
        Some(Command::Commit) => commit::run()?,
        Some(Command::Abort) => abort::run()?,
        Some(Command::Rule { action }) => match action {
            RuleAction::Add { path, perm } => config::add_rule(&path, &perm)?,
            RuleAction::Remove { path } => config::remove_rule(&path)?,
        },
        Some(Command::Log { follow, dump }) => log::run(follow, dump)?,
        Some(Command::Watch) => watch::run()?,
        None => return run(&cli.exec_args),
    }

    Ok(0)
}

/// Full workflow: mount (if needed) → watch → exec → diff → commit/abort/stage.
fn run(exec_args: &[String]) -> anyhow::Result<u8> {
    // 1. Mount if not already mounted
    mount::run()?;

    // 2. Start background watch daemon (prompts for permission asks)
    watch::run_background()?;

    // 3. Exec (spawn + wait) — continue to diff even if command fails
    let cmd_exit_code = exec::run(exec_args).unwrap_or(1);

    // 4. Show diff
    let has_changes = diff::run()?;

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
        "c" | "commit" => commit::run()?,
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
