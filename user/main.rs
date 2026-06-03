// yolo CLI — main.rs

use clap::{Parser, Subcommand};
use colored::Colorize;
use yolofs::cmd::{
    abort, audit, commit, diff, exec, init, load, mount, snapshot, timeline, travel, watch,
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
    /// Show staged changes (latest snapshot by default; --full for all)
    Status {
        /// Show state at a named snapshot (single segment)
        #[arg(long)]
        at: Option<String>,
        /// Start from a named snapshot (inclusive)
        #[arg(long, conflicts_with = "at")]
        from: Option<String>,
        /// End at a named snapshot (inclusive)
        #[arg(long, conflicts_with = "at")]
        to: Option<String>,
        /// Show all staged changes since base (not just the latest snapshot)
        #[arg(long, conflicts_with_all = ["at", "from", "to"])]
        full: bool,
    },
    /// Git-style diff of staged vs base (latest snapshot by default; --full for all)
    Diff {
        /// Diff a single snapshot segment
        #[arg(long)]
        at: Option<String>,
        /// Diff changes since a named snapshot
        #[arg(long, conflicts_with = "at")]
        from: Option<String>,
        /// Diff changes up to a named snapshot
        #[arg(long, conflicts_with = "at")]
        to: Option<String>,
        /// Diff all staged changes since base (not just the latest snapshot)
        #[arg(long, conflicts_with_all = ["at", "from", "to"])]
        full: bool,
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
    /// Show session history (latest snapshot by default; --full for all)
    Audit {
        /// Filter to operations on a specific path
        #[arg(long)]
        path: Option<String>,
        /// Show the entire journal (not just the latest snapshot)
        #[arg(long)]
        full: bool,
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
        return run_and_review(&raw[pos + 1..]);
    }

    let cli = Cli::parse();

    // yolo is a host-side tool: its base-fs operations only work outside the
    // mount, so refuse every subcommand when run inside the chroot. Sandbox
    // commands with `yolo exec -- <cmd>` and manage from outside.
    if cli.command.is_some() && yolofs::utils::inside_mount() {
        anyhow::bail!("yolo cannot run inside the mount — run it from outside");
    }

    match cli.command {
        Some(Command::Init { agents }) => init::run(&std::env::current_dir()?, &agents)?,
        Some(Command::Load) => {
            if !load::load()? {
                eprintln!("{} kernel module already loaded", "yolo:".green());
            }
        }
        Some(Command::Unload) => load::unload()?,
        Some(Command::Reload) => load::reload()?,
        Some(Command::Mount) => mount::mount()?,
        Some(Command::Exec { exec_args }) => return exec::run(&exec_args),
        Some(Command::Unmount { force }) => mount::unmount(force)?,
        Some(Command::Remount { force }) => mount::remount(force)?,
        Some(Command::Status { at, from, to, full }) => {
            diff::run_status(at.as_deref(), from.as_deref(), to.as_deref(), full)?
        }
        Some(Command::Diff {
            at,
            from,
            to,
            full,
            path,
        }) => {
            diff::run_diff(
                at.as_deref(),
                from.as_deref(),
                to.as_deref(),
                path.as_deref(),
                full,
            )?;
        }
        Some(Command::Commit) => commit::run()?,
        Some(Command::Abort { force }) => abort::run(force)?,
        Some(Command::Snapshot { name, if_changed }) => {
            snapshot::create(name.as_deref(), if_changed)?;
        }
        Some(Command::Travel { name }) => travel::run(&name)?,
        Some(Command::Timeline) => timeline::run()?,
        Some(Command::Audit { path, full }) => audit::run(path.as_deref(), full)?,
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
    let code = exec::run(run_args)?;
    diff::run_status(None, None, None, false)?;
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
            ("status",   "Show staged changes (latest snapshot; --full for all)"),
            ("diff",     "Git-style diff of staged vs base"),
            ("commit",   "Apply staged changes to base"),
            ("abort",    "Discard staged changes"),
        ]),
        ("History", &[
            ("snapshot", "Create a snapshot"),
            ("travel",   "Travel to a previous snapshot"),
            ("timeline", "Show the snapshot/travel timeline"),
            ("audit",    "Show session history (latest snapshot; --full for all)"),
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
