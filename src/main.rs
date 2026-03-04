//! agfs - Run commands in a sandboxed overlay filesystem.

mod changes;
mod executor;
mod sandbox;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use changes::{commit_changes, show_summary};
use executor::run_in_sandbox;
use sandbox::Sandbox;

#[derive(Parser)]
#[command(
    name = "agfs",
    version,
    about = "Run commands in a sandboxed overlay filesystem"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Don't commit or prompt for commit
    #[arg(short = 'n', long)]
    no_commit: bool,

    /// Assume yes to commit prompt
    #[arg(short = 'y', long)]
    yes: bool,

    /// Use existing sandbox directory
    #[arg(short = 'D', long, value_name = "DIR")]
    sandbox_dir: Option<PathBuf>,

    /// Disable network access
    #[arg(short = 'x', long)]
    no_network: bool,

    /// Command to run
    #[arg(trailing_var_arg = true)]
    args: Vec<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Show summary of changes in a sandbox
    Summary { dir: PathBuf },
    /// Commit changes from a sandbox
    Commit { dir: PathBuf },
    /// Explore a sandbox interactively
    Explore { dir: Option<PathBuf> },
}

fn prompt_commit() -> bool {
    print!("\nCommit these changes? [y/N] ");
    io::stdout().flush().unwrap();
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Summary { dir }) => {
            let sandbox = Sandbox::new_at(dir)?;
            if !sandbox.upperdir.exists() {
                bail!("Not a valid sandbox directory");
            }
            show_summary(&sandbox)?;
            Ok(ExitCode::SUCCESS)
        }

        Some(Commands::Commit { dir }) => {
            let sandbox = Sandbox::new_at(dir)?;
            if !sandbox.upperdir.exists() {
                bail!("Not a valid sandbox directory");
            }
            commit_changes(&sandbox)?;
            Ok(ExitCode::SUCCESS)
        }

        Some(Commands::Explore { dir }) => {
            let sandbox = match dir {
                Some(d) => Sandbox::new_at(d)?,
                None => Sandbox::new_temp()?,
            };
            println!("{}: {}", "Sandbox".cyan(), sandbox.root.display());
            let exit_code = run_in_sandbox(&sandbox, &[], cli.no_network)?;
            if !cli.no_commit && show_summary(&sandbox)? {
                if cli.yes || prompt_commit() {
                    commit_changes(&sandbox)?;
                } else {
                    println!("Not committing. Sandbox at: {}", sandbox.root.display());
                }
            }
            Ok(ExitCode::from(exit_code as u8))
        }

        None => {
            if cli.args.is_empty() {
                bail!("No command specified. Use --help for usage.");
            }
            let sandbox = match cli.sandbox_dir {
                Some(d) => Sandbox::new_at(d)?,
                None => Sandbox::new_temp()?,
            };
            if !sandbox.is_valid() {
                bail!("Invalid sandbox directory");
            }
            println!("{}: {}", "Sandbox".cyan(), sandbox.root.display());
            let exit_code = run_in_sandbox(&sandbox, &cli.args, cli.no_network)?;
            if cli.no_commit {
                println!("{}", sandbox.root.display());
            } else if show_summary(&sandbox)? {
                if cli.yes || prompt_commit() {
                    commit_changes(&sandbox)?;
                } else {
                    println!("Not committing. Sandbox at: {}", sandbox.root.display());
                }
            }
            Ok(ExitCode::from(exit_code as u8))
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{}: {}", "Error".red().bold(), e);
            ExitCode::FAILURE
        }
    }
}
