// yolo CLI — main.rs

use clap::{Parser, Subcommand};
use colored::Colorize;
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
    // ── Setup ────────────────────────────────────────────────────────
    /// Create yolofs.toml and scaffold agent hook templates
    Init {
        /// Agent hooks to scaffold (e.g. `--agents claude gemini`). Repeatable.
        /// Omit to scaffold every supported agent.
        #[arg(long = "agents", num_args = 1.., ignore_case = true)]
        agents: Vec<init::AgentChoice>,
    },
    /// Load the kernel module
    Load,
    /// Unmount all sessions and unload the kernel module
    Unload,
    /// Unload then reload the kernel module
    Reload,
    // ── Session ──────────────────────────────────────────────────────
    /// Create .yolofs/ layout and mount the filesystem
    Mount,
    /// Execute a command under yolofs (requires existing mount)
    Exec {
        /// Command to run (after --)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        exec_args: Vec<String>,
    },
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
    // ── Review & commit ──────────────────────────────────────────────
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
        /// Limit to a single file, passed after `--` (e.g. `-- foo.txt`)
        #[arg(last = true)]
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
        /// Snapshot name or numeric ID
        name: String,
    },
    /// Show snapshot/travel timeline (unreachable branches dimmed)
    Timeline,
    /// Raw journal: every op + access note over a range (vs `review`'s net view)
    Journal {
        /// Snapshot id or range (see `review`); `all` = the full log
        range: Option<String>,
        /// Limit to ops on a single file, passed after `--` (e.g. `-- foo.txt`)
        #[arg(last = true)]
        path: Option<String>,
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
    /// Allow read + execute, deny write
    Read { path: String },
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
    // `yolo -- <cmd>`: when `--` is the first argument (no subcommand before
    // it), run <cmd> under yolofs and then show what changed. Handled before
    // clap so a bare unknown word (e.g. `yolo notacommand`) still falls through
    // to clap's help instead of being treated as a command to run.
    let raw: Vec<String> = std::env::args().collect();
    if let Some(pos) = raw.iter().position(|a| a == "--")
        && raw[1..pos].is_empty()
        && pos + 1 < raw.len()
    {
        if yolofs::utils::inside_mount() {
            anyhow::bail!("yolo cannot run inside the mount — run it from outside");
        }
        let cmd = &raw[pos + 1..];
        // A `yolo` subcommand is host-side, so it can't be sandboxed — gate it
        // instead (the agent may inspect/navigate; commit/abort/rule/… are the
        // human's). Everything else runs sandboxed, then we show what changed.
        if cmd.first().map(String::as_str) == Some("yolo") {
            return run_agent_yolo(cmd);
        }
        return run_and_review(cmd);
    }

    let cli = Cli::parse();

    // yolo is a host-side tool: its base-fs operations only work outside the
    // mount, so refuse every subcommand when run inside the chroot. Sandbox
    // commands with `yolo exec -- <cmd>` and manage from outside.
    if cli.command.is_some() && yolofs::utils::inside_mount() {
        anyhow::bail!("yolo cannot run inside the mount — run it from outside");
    }

    dispatch(cli.command)
}

/// Subcommands the agent may run via its hook's `yolo -- yolo <sub>` — read and
/// navigation only. Everything else (commit/abort/rule/session control/…) is the
/// human's, so this is default-deny: an unlisted or unknown subcommand is
/// rejected. Centralizing the policy here keeps agent hooks trivial
/// (`yolo -- <cmd>`) and prevents them from drifting.
const AGENT_ALLOWED: &[&str] = &["review", "journal", "timeline", "travel", "snapshot"];

/// The agent invoked `yolo <sub> …` (its hook wraps every command as
/// `yolo -- <cmd>`). `yolo` is host-side, so the allowed ones run directly — not
/// sandboxed — and the rest are rejected.
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
        Some(Command::Init { agents }) => init::run(&std::env::current_dir()?, &agents)?,
        Some(Command::Load) => {
            if !load::load()? {
                eprintln!("{} kernel module already loaded", "yolo:".green());
            }
        }
        Some(Command::Unload) => load::unload()?,
        Some(Command::Reload) => load::reload()?,
        Some(Command::Mount) => mount::mount()?,
        Some(Command::Exec { exec_args }) => {
            let (code, snapshot) = exec::run(&exec_args)?;
            exec::announce(&snapshot);
            return Ok(code);
        }
        Some(Command::Unmount { force }) => mount::unmount(force)?,
        Some(Command::Remount { force }) => mount::remount(force)?,
        Some(Command::Review {
            range,
            diff,
            each,
            path,
        }) => review::run_review(range.as_deref(), path.as_deref(), each, diff)?,
        Some(Command::Commit) => commit::run()?,
        Some(Command::Abort { force }) => abort::run(force)?,
        Some(Command::Snapshot { name, if_changed }) => {
            snapshot::create(name.as_deref(), if_changed)?;
        }
        Some(Command::Travel { name }) => travel::run(&name)?,
        Some(Command::Timeline) => timeline::run()?,
        Some(Command::Journal { range, path }) => journal::run(range.as_deref(), path.as_deref())?,
        Some(Command::Rule { action }) => match action {
            RuleAction::List => config::list_rules()?,
            RuleAction::Resolve { path } => config::resolve_rule(&path)?,
            RuleAction::Unset { path } => config::unset_rule(&path)?,
            RuleAction::Ask { path } => config::set_rule(&path, perm::Perm::Ask)?,
            RuleAction::Allow { path } => config::set_rule(&path, perm::Perm::Allow)?,
            RuleAction::Read { path } => config::set_rule(&path, perm::Perm::Read)?,
            RuleAction::Deny { path } => config::set_rule(&path, perm::Perm::Deny)?,
            RuleAction::Hide { path } => config::set_rule(&path, perm::Perm::Hide)?,
        },
        Some(Command::Watch { allow_all }) => watch::run(allow_all)?,
        None => print_overview(),
    }

    Ok(0)
}

/// `yolo -- <cmd>`: run a command under yolofs (like `exec`), then print a
/// status summary of what it changed — the friendly counterpart to the quiet
/// `exec`. Returns the command's exit code.
fn run_and_review(run_args: &[String]) -> anyhow::Result<u8> {
    let (code, snapshot) = exec::run(run_args)?;
    let snapshot_id = match snapshot {
        exec::Snapshot::Created(gen_id) => Some(gen_id),
        exec::Snapshot::NoChanges | exec::Snapshot::Off => None,
    };
    review::run_after_exec(snapshot_id)?;
    Ok(code)
}

/// Bare `yolo`: a grouped overview of the commands. (`yolo --help` still prints
/// clap's full flat reference.)
fn print_overview() {
    #[rustfmt::skip]
    let groups: &[(&str, &[(&str, &str)])] = &[
        ("Setup", &[
            ("init",     "Create yolofs.toml + agent hook templates"),
            ("load",     "Load the kernel module"),
            ("unload",   "Unmount all sessions and unload the module"),
            ("reload",   "Unload then reload the kernel module"),
        ]),
        ("Session", &[
            ("mount",    "Create .yolofs/ and mount the filesystem"),
            ("-- <cmd>", "Run <cmd> under yolofs and show what changed"),
            ("exec",     "Run a command under yolofs, quietly (auto-snapshots)"),
            ("unmount",  "Tear down the session"),
            ("remount",  "Unmount then remount (picks up yolofs.toml options)"),
        ]),
        ("Review & commit", &[
            ("review",   "Review staged changes (summary; `--diff` for the diff)"),
            ("commit",   "Apply staged changes to base"),
            ("abort",    "Discard staged changes"),
        ]),
        ("History", &[
            ("snapshot", "Create a snapshot"),
            ("travel",   "Travel to a previous snapshot"),
            ("timeline", "Show the snapshot/travel timeline"),
            ("journal",  "Raw record log over a range (`-- path` to filter)"),
        ]),
        ("Permissions", &[
            ("rule",     "Manage permission rules (allow/read/deny/hide/ask)"),
            ("watch",    "Permission-prompt daemon"),
        ]),
    ];

    println!(
        "{}",
        "Agentic filesystem — staging-commit + permission gating".bold()
    );
    println!();
    println!(
        "{}",
        "Run yolo outside the mount; sandbox your work with `yolo exec`.".dimmed()
    );
    for (heading, cmds) in groups {
        println!("\n{}", heading.cyan().bold());
        for (name, desc) in *cmds {
            println!("  {} {}", format!("{name:<9}").bold(), desc.dimmed());
        }
    }
    println!("\n{}", "Run `yolo <command> --help` for details.".dimmed());
}
