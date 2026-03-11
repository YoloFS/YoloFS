// agfs CLI — init.rs
//
// `agfs init` — create agfs.toml in the current directory.

use anyhow::{Context, Result};
use colored::Colorize;
use std::env;
use std::fs;

pub const DEFAULT_CONFIG: &str = r#"[mount]
ask_timeout = 0
ask_default = "deny"

[rules]
"#;

pub fn run() -> Result<()> {
    let cwd = env::current_dir().context("getting cwd")?;
    let config_path = cwd.join("agfs.toml");

    if config_path.exists() {
        eprintln!("{}", "agfs.toml already exists".yellow());
        return Ok(());
    }

    fs::write(&config_path, DEFAULT_CONFIG).context("writing agfs.toml")?;
    eprintln!("{} {}", "created".green().bold(), config_path.display());
    Ok(())
}
