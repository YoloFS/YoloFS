// yolo CLI — main.rs

use clap::{CommandFactory, Parser, Subcommand};
use colored::Colorize;
use std::io::{self, BufRead, Write};
use yolofs::cmd::{
    abort, audit, checkpoint, commit, diff, exec, init, load, mount, restore, timeline, watch,
};
use yolofs::config;

#[derive(Parser)]
#[command(
    name = "yolo",
    about = "Agentic filesystem — staging-commit + permission gating"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Automatically allow all permission requests without prompting
    #[arg(long)]
    allow_all: bool,

    /// Command to run inside the sandbox (after --)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    exec_args: Vec<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Load the kernel module
    Load,
    /// Unmount all sessions and unload the kernel module
    Unload,
    /// Unload then reload the kernel module
    Reload,
    /// Create yolofs.toml and scaffold agent hook templates
    Init {
        /// Agent hooks to scaffold (e.g. `--agents claude gemini`). Repeatable.
        /// Omit to scaffold every supported agent.
        #[arg(long = "agents", num_args = 1.., ignore_case = true)]
        agents: Vec<init::AgentChoice>,
    },
    /// Create .yolofs/ layout and mount the filesystem
    Mount,
    /// Unmount and clean up the session
    Unmount {
        /// Skip staged-changes prompt
        #[arg(long, short)]
        force: bool,
    },
    /// Unmount then remount (picks up new yolofs.toml mount options)
    Remount {
        /// Skip staged-changes prompt
        #[arg(long, short)]
        force: bool,
    },
    /// Execute a command inside the sandbox (requires existing mount)
    Exec {
        /// Command to run (after --)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        exec_args: Vec<String>,
    },
    /// Show staged changes
    Status {
        /// Show state at a named checkpoint (single segment)
        #[arg(long)]
        at: Option<String>,
        /// Start from a named checkpoint (inclusive)
        #[arg(long, conflicts_with = "at")]
        from: Option<String>,
        /// End at a named checkpoint (inclusive)
        #[arg(long, conflicts_with = "at")]
        to: Option<String>,
    },
    /// Git-style diff of staged vs base
    Diff {
        /// Diff a single checkpoint segment
        #[arg(long)]
        at: Option<String>,
        /// Diff changes since a named checkpoint
        #[arg(long, conflicts_with = "at")]
        from: Option<String>,
        /// Diff changes up to a named checkpoint
        #[arg(long, conflicts_with = "at")]
        to: Option<String>,
        /// Show diff for a single file
        path: Option<String>,
    },
    /// Apply staged changes to base
    Commit,
    /// Discard staged changes
    Abort {
        /// Skip confirmation prompt
        #[arg(long, short)]
        force: bool,
    },
    /// Create a checkpoint
    Checkpoint {
        /// Checkpoint name (defaults to timestamp)
        name: Option<String>,
    },
    /// Restore to a previous checkpoint
    Restore {
        /// Checkpoint name or numeric ID
        name: String,
    },
    /// Show checkpoint/restore timeline (unreachable branches dimmed)
    Timeline,
    /// Show full session history (every operation, dead branches dimmed)
    Audit {
        /// Filter to operations on a specific path
        #[arg(long)]
        path: Option<String>,
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
        /// Permission: allow, ro, deny, hidden, ask
        perm: String,
    },
    /// Remove a permission rule
    Remove {
        /// Path to remove rule from
        path: String,
    },
}

fn main() -> ! {
    colored::control::set_override(true);
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
        Some(Command::Load) => {
            if !load::load()? {
                eprintln!("{} kernel module already loaded", "yolofs:".green());
            }
        }
        Some(Command::Unload) => load::unload()?,
        Some(Command::Reload) => load::reload()?,
        Some(Command::Init { agents }) => init::run(&std::env::current_dir()?, &agents)?,
        Some(Command::Mount) => mount::mount()?,
        Some(Command::Unmount { force }) => mount::unmount(force)?,
        Some(Command::Remount { force }) => mount::remount(force)?,
        Some(Command::Exec { exec_args }) => return exec::run(&exec_args),
        Some(Command::Status { at, from, to }) => {
            diff::run_status(at.as_deref(), from.as_deref(), to.as_deref())?
        }
        Some(Command::Diff { at, from, to, path }) => {
            diff::run_diff(
                at.as_deref(),
                from.as_deref(),
                to.as_deref(),
                path.as_deref(),
            )?;
        }
        Some(Command::Commit) => commit::run()?,
        Some(Command::Abort { force }) => abort::run(force)?,
        Some(Command::Checkpoint { name }) => {
            checkpoint::create(name.as_deref())?;
        }
        Some(Command::Restore { name }) => restore::run(&name)?,
        Some(Command::Timeline) => timeline::run()?,
        Some(Command::Audit { path }) => audit::run(path.as_deref())?,
        Some(Command::Rule { action }) => match action {
            RuleAction::Add { path, perm } => config::add_rule(&path, &perm)?,
            RuleAction::Remove { path } => config::remove_rule(&path)?,
        },
        Some(Command::Watch { allow_all }) => watch::run(allow_all)?,
        None => {
            let has_separator = std::env::args().any(|a| a == "--");
            if !cli.exec_args.is_empty() && !has_separator {
                Cli::command().print_help()?;
                std::process::exit(1);
            }
            return run(&cli.exec_args, cli.allow_all);
        }
    }

    Ok(0)
}

/// Full workflow: mount (if needed) → watch → exec → diff → commit/abort/stage.
fn run(exec_args: &[String], allow_all: bool) -> anyhow::Result<u8> {
    // 1. Mount if not already mounted
    mount::mount()?;

    // 2. Start background watch daemon (prompts for permission asks)
    watch::run_background(allow_all)?;

    // 3. Exec (spawn + wait) — continue to diff even if command fails
    let cmd_exit_code = exec::run(exec_args).unwrap_or(1);

    // 4. Show diff
    let has_changes = diff::run_diff(None, None, None, None)?;

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
        "a" | "abort" => abort::run(true)?,
        _ => {
            eprintln!(
                "{}",
                "Changes kept staged. Run `yolo status` or `yolo diff` to review, `yolo commit` to apply, `yolo abort` to discard.".cyan()
            );
        }
    }

    Ok(cmd_exit_code)
}
