//! agfs - Run commands in a sandboxed overlay filesystem.

mod changes;
mod executor;

use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use changes::{commit_changes, show_summary};
use executor::{Sandbox, run_in_sandbox};

#[derive(Parser)]
#[command(
    name = "agfs",
    version,
    about = "Run commands in a sandboxed overlay filesystem"
)]
struct Cli {
    /// Use sandbox directory (default: ./.staging)
    #[arg(short = 'D', long, value_name = "DIR")]
    sandbox_dir: Option<PathBuf>,

    /// Command to run
    #[arg(trailing_var_arg = true)]
    args: Vec<String>,
}

fn prompt_commit() -> bool {
    print!("\nCommit these changes? [y/N] ");
    io::stdout().flush().unwrap();
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

fn default_sandbox_dir() -> Result<PathBuf> {
    Ok(std::env::current_dir()?.join(".staging"))
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();

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
        if prompt_commit() {
            commit_changes(&sandbox)?;
        } else {
            println!("Not committing. Sandbox at: {}", sandbox.root.display());
        }
    }

    Ok(ExitCode::from(exit_code as u8))
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
