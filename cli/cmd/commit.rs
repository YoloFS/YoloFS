// agfs CLI — commit.rs
//
// `agfs commit` — apply staged changes to base.
// Journal is compacted first, then actions are applied sequentially.

use crate::journal;
use crate::journal::SegmentedJournal;
use anyhow::Result;
use colored::Colorize;

pub fn run() -> Result<()> {
    let agfs = crate::utils::session_dir()?;

    let sj = SegmentedJournal::new(journal::read(&agfs)?);
    let actions = journal::compact::compact(sj.live().into_records());
    let changeset = actions.collapse();

    if changeset.0.is_empty() {
        println!("{}", "Nothing to commit.".yellow());
        return Ok(());
    }

    let committed = changeset.0.len();

    actions.apply(&agfs)?;

    super::abort::reset_staging(&agfs)?;

    println!(
        "{}",
        format!(
            "Committed {committed} change{}.",
            crate::utils::plural(committed)
        )
        .green()
        .bold()
    );

    Ok(())
}
