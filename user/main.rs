// yolo CLI — main.rs

use clap::{Parser, Subcommand};
use colored::Colorize;
use yolofs::AGENT_ALLOWED;
use yolofs::cmd::{
    abort, commit, exec, init, journal, load, mount, review, snapshot, timeline, travel, watch,
};
use yolofs::config;
use yolofs::perm;

#[derive(Parser)]
#[command(
    name = "yolo",
    about = "Agentic filesystem — staging-commit + permission gating"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    // ── Workflow ─────────────────────────────────────────────────────
    /// Create yolofs.toml and scaffold agent hook templates
    Init {
        /// Project directory to initialize (created if missing). Defaults to the
        /// current directory.
        #[arg(default_value = ".")]
        path: std::path::PathBuf,
        /// Agent hooks to scaffold (e.g. `--agents claude gemini`). Repeatable.
        /// Omit to scaffold every supported agent.
        #[arg(long = "agents", num_args = 1.., ignore_case = true)]
        agents: Vec<init::AgentChoice>,
    },
    /// Run a command under yolofs, mounting on first run, then review it
    Run {
        /// Skip the review summary; emit only the terse snapshot line (stderr)
        #[arg(long)]
        no_review: bool,
        /// Command to run, after `--` (e.g. `yolo run -- make build`)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        exec_args: Vec<String>,
    },
    /// Review staged changes — a summary, or a git-style diff with `--diff`
    Review {
        /// Snapshot id or range: `N` = snapshot N's own change; `a..b` =
        /// between two; `a..` / `..b` open ends; `all` (or `..`) = everything
        /// vs base. Ids are numbers — see `yolo timeline`.
        range: Option<String>,
        /// Show a git-style unified diff instead of the one-line summary
        #[arg(long)]
        diff: bool,
        /// One stanza per consecutive snapshot in the range
        #[arg(long)]
        each: bool,
    },
    /// Apply staged changes to base
    Commit,
    /// Discard staged changes
    Abort {
        /// Skip confirmation prompt
        #[arg(long, short)]
        force: bool,
    },
    // ── Permissions ──────────────────────────────────────────────────
    /// Manage permission rules
    #[command(arg_required_else_help = true)]
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
    // ── History ──────────────────────────────────────────────────────
    /// Create a snapshot
    Snapshot {
        /// Snapshot name (defaults to timestamp)
        name: Option<String>,
        /// Only snapshot if there are staged changes (no-op otherwise)
        #[arg(long)]
        if_changed: bool,
    },
    /// Travel to a previous snapshot
    Travel {
        /// Snapshot/travel generation id (see `yolo timeline`)
        id: String,
    },
    /// Show snapshot/travel timeline (unreachable branches dimmed)
    Timeline,
    /// Raw journal: every op + audit note over a range (vs `review`'s net view)
    Journal {
        /// Snapshot id or range (see `review`); `all` = the full log
        range: Option<String>,
        /// Limit to ops on a single file, passed after `--` (e.g. `-- foo.txt`)
        #[arg(last = true)]
        path: Option<String>,
    },
    // ── Manual control ───────────────────────────────────────────────
    /// Create .yolofs/ layout and mount the filesystem
    Mount,
    /// Unmount the live view, preserving staged state
    Unmount,
    /// Unmount then remount (picks up new yolofs.toml mount options)
    Remount,
    /// Load the kernel module
    Load,
    /// Unmount all sessions and unload the kernel module
    Unload,
    /// Unload then reload the kernel module
    Reload,
}

/// Each mutating verb names a permission state; `list`/`show` are queries.
#[derive(Subcommand)]
enum RuleAction {
    /// Remove the rule on a path (revert to inheriting from ancestors)
    Unset { path: String },
    /// Prompt on access, overriding any inherited rule
    Ask { path: String },
    /// Allow read + write + execute
    Allow { path: String },
    /// Allow read + execute, prompt before writes
    WriteAsk { path: String },
    /// Allow read + execute, deny writes
    ReadOnly { path: String },
    /// Deny all access
    Deny { path: String },
    /// Deny access and hide the path (ENOENT)
    Hide { path: String },
    /// List all configured rules
    List,
    /// Resolve the effective permission for a path (and where it comes from)
    Resolve { path: String },
}

fn main() -> ! {
    let code = match run_cli() {
        Ok(code) => code,
        Err(e) => {
            // `:#` renders the whole context chain on one line.
            yolofs::report::error(format!("{e:#}"));
            1
        }
    };
    std::process::exit(code as i32);
}

fn run_cli() -> anyhow::Result<u8> {
    let cli = Cli::parse();

    // yolo is a host-side tool: its base-fs operations only work outside the
    // mount, so refuse every subcommand when run inside it (a command whose root
    // was pivoted onto the mount by `yolo run`). Run commands through
    // `yolo run -- <cmd>` and manage from outside.
    if cli.command.is_some() && yolofs::utils::inside_mount() {
        anyhow::bail!("yolo cannot run inside the mount — run it from outside");
    }

    dispatch(cli.command)
}

/// The agent ran `yolo <sub> …` (its hook wraps every command as
/// `yolo run -- <cmd>`, so a `yolo` command arrives here as `run`'s exec_args).
/// `yolo` is host-side, so the allowed ones run directly — not in the overlay —
/// and the rest are rejected.
fn run_agent_yolo(cmd: &[String]) -> anyhow::Result<u8> {
    match cmd.get(1).map(String::as_str) {
        Some(sub) if AGENT_ALLOWED.contains(&sub) => {
            dispatch(Cli::parse_from(cmd.iter().map(String::as_str)).command)
        }
        other => anyhow::bail!(
            "`yolo {}` is reserved for the human; the agent may run: {}",
            other.unwrap_or(""),
            AGENT_ALLOWED.join(", ")
        ),
    }
}

/// Dispatch a parsed command to its handler. Returns the process exit code.
fn dispatch(command: Option<Command>) -> anyhow::Result<u8> {
    match command {
        Some(Command::Init { path, agents }) => init::run(&path, &agents)?,
        Some(Command::Load) => {
            if !load::load()? {
                yolofs::report::hint("kernel module already loaded");
            }
        }
        Some(Command::Unload) => load::unload()?,
        Some(Command::Reload) => load::reload()?,
        Some(Command::Mount) => mount::mount()?,
        Some(Command::Run {
            no_review,
            exec_args,
        }) => {
            if exec_args.is_empty() {
                anyhow::bail!("no command given — usage: `yolo run -- <cmd>`");
            }
            // A `yolo` subcommand is host-side, so it can't run in the staging
            // overlay — gate it instead (the agent may inspect/navigate;
            // commit/abort/rule/… are the human's). Everything else runs in the
            // overlay, then we show what changed.
            return if exec_args.first().map(String::as_str) == Some("yolo") {
                run_agent_yolo(&exec_args)
            } else {
                run_and_review(&exec_args, no_review)
            };
        }
        Some(Command::Unmount) => mount::unmount()?,
        Some(Command::Remount) => mount::remount()?,
        Some(Command::Review { range, diff, each }) => {
            review::run_review(range.as_deref(), each, diff)?
        }
        Some(Command::Commit) => commit::run()?,
        Some(Command::Abort { force }) => abort::run(force)?,
        Some(Command::Snapshot { name, if_changed }) => {
            snapshot::create(name.as_deref(), if_changed)?;
        }
        Some(Command::Travel { id }) => travel::run(&id)?,
        Some(Command::Timeline) => timeline::run()?,
        Some(Command::Journal { range, path }) => journal::run(range.as_deref(), path.as_deref())?,
        Some(Command::Rule { action }) => match action {
            RuleAction::List => config::list_rules()?,
            RuleAction::Resolve { path } => config::resolve_rule(&path)?,
            RuleAction::Unset { path } => config::unset_rule(&path)?,
            RuleAction::Ask { path } => config::set_rule(&path, perm::Perm::Ask)?,
            RuleAction::Allow { path } => config::set_rule(&path, perm::Perm::Allow)?,
            RuleAction::WriteAsk { path } => config::set_rule(&path, perm::Perm::WriteAsk)?,
            RuleAction::ReadOnly { path } => config::set_rule(&path, perm::Perm::ReadOnly)?,
            RuleAction::Deny { path } => config::set_rule(&path, perm::Perm::Deny)?,
            RuleAction::Hide { path } => config::set_rule(&path, perm::Perm::Hide)?,
        },
        Some(Command::Watch { allow_all }) => watch::run(allow_all)?,
        None => print_overview(),
    }

    Ok(0)
}

/// `yolo run [--no-review] -- <cmd>`: run a command under yolofs, then print a
/// status summary of what it changed. `--no-review` emits only the terse
/// snapshot line. Returns the command's exit code.
fn run_and_review(run_args: &[String], no_review: bool) -> anyhow::Result<u8> {
    let (code, snapshot) = exec::run(run_args)?;
    if no_review {
        exec::announce(&snapshot);
    } else {
        let snapshot_id = match snapshot {
            exec::Snapshot::Created(gen_id) => Some(gen_id),
            exec::Snapshot::NoChanges | exec::Snapshot::Off => None,
        };
        review::run_after_exec(snapshot_id)?;
    }
    Ok(code)
}

/// Bare `yolo`: a grouped overview of the commands. (`yolo --help` still prints
/// clap's full flat reference.)
fn print_overview() {
    #[rustfmt::skip]
    let groups: &[(&str, &[(&str, &str)])] = &[
        ("Workflow", &[
            ("init",     "Create yolofs.toml + agent hook templates"),
            ("run",      "Run under yolofs, mounting on first run (`--no-review` skips review)"),
            ("review",   "Review staged changes (summary; `--diff` for the diff)"),
            ("commit",   "Apply staged changes to base"),
            ("abort",    "Discard staged changes"),
        ]),
        ("Permissions", &[
            ("rule",     "Manage permission rules (allow/write-ask/read-only/ask/deny/hide)"),
            ("watch",    "Permission-prompt daemon"),
        ]),
        ("History", &[
            ("snapshot", "Create a snapshot"),
            ("travel",   "Travel to a previous snapshot"),
            ("timeline", "Show the snapshot/travel timeline"),
            ("journal",  "Raw record log (low-level; `timeline` is the curated view)"),
        ]),
        ("Manual control", &[
            ("mount",    "Create .yolofs/ and mount the filesystem"),
            ("unmount",  "Tear down the live view, preserving staged state"),
            ("remount",  "Rebuild the view while preserving staged work"),
            ("load",     "Load the kernel module"),
            ("unload",   "Unmount all sessions and unload the module"),
            ("reload",   "Unload then reload the kernel module"),
        ]),
    ];

    println!(
        "{}",
        "Agentic filesystem — staging-commit + permission gating".bold()
    );
    println!();
    println!(
        "{}",
        "Run yolo outside the mount; stage your work with `yolo run -- <cmd>`.".dimmed()
    );
    for (heading, cmds) in groups {
        println!("\n{}", heading.cyan().bold());
        for (name, desc) in *cmds {
            println!("  {} {}", format!("{name:<9}").bold(), desc.dimmed());
        }
    }
    println!("\n{}", "Run `yolo <command> --help` for details.".dimmed());
}
