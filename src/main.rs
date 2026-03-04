//! agfs - Run commands in a sandboxed overlay filesystem.

mod changes;
mod executor;
mod mcp;

use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use changes::{commit_changes, show_summary};
use executor::{Sandbox, destroy_sandbox, run_in_sandbox};
use mcp::serve_mcp;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChangeAction {
    Commit,
    Abort,
    Stage,
}

#[derive(Parser)]
#[command(
    name = "agfs",
    version,
    about = "Run commands in a sandboxed overlay filesystem"
)]
struct Cli {
    /// Run as an MCP stdio server
    #[arg(long)]
    mcp: bool,

    /// Use sandbox directory (default: ./.staging)
    #[arg(short = 'D', long, value_name = "DIR")]
    sandbox_dir: Option<PathBuf>,

    /// Command to run
    #[arg(trailing_var_arg = true)]
    args: Vec<String>,
}

fn parse_change_action(input: &str) -> Option<ChangeAction> {
    match input.trim().to_lowercase().as_str() {
        "c" | "commit" => Some(ChangeAction::Commit),
        "a" | "abort" => Some(ChangeAction::Abort),
        "" | "s" | "stage" => Some(ChangeAction::Stage),
        _ => None,
    }
}

fn prompt_change_action() -> ChangeAction {
    loop {
        print!("\nChoose [c]ommit, [a]bort, or [s]tage [default: stage]: ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();

        if let Some(action) = parse_change_action(&input) {
            return action;
        }

        println!("Enter commit, abort, or stage.");
    }
}

fn abort_changes(sandbox: &Sandbox) -> Result<()> {
    destroy_sandbox(sandbox)?;
    println!("Aborted. Removed sandbox: {}", sandbox.root.display());
    Ok(())
}

fn default_sandbox_dir() -> Result<PathBuf> {
    Ok(std::env::current_dir()?.join(".staging"))
}

async fn run() -> Result<ExitCode> {
    let cli = Cli::parse();

    if cli.mcp {
        if !cli.args.is_empty() {
            anyhow::bail!("--mcp cannot be used with a command");
        }
        serve_mcp().await?;
        return Ok(ExitCode::SUCCESS);
    }

    let sandbox = match cli.sandbox_dir {
        Some(d) => Sandbox::new_at(d)?,
        None => Sandbox::new_at(default_sandbox_dir()?)?,
    };
    println!("{}: {}", "Sandbox".cyan(), sandbox.root.display());

    let exit_code = if cli.args.is_empty() {
        0
    } else {
        run_in_sandbox(&sandbox, &cli.args)?
    };

    if show_summary(&sandbox)? {
        match prompt_change_action() {
            ChangeAction::Commit => commit_changes(&sandbox)?,
            ChangeAction::Abort => abort_changes(&sandbox)?,
            ChangeAction::Stage => {
                println!("Staged. Sandbox at: {}", sandbox.root.display());
            }
        }
    }

    Ok(ExitCode::from(exit_code as u8))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{}: {}", "Error".red().bold(), e);
            ExitCode::FAILURE
        }
    }
}
