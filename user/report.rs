// yolo CLI — report.rs
//
// Unified status reporting. Every status line the CLI emits — progress,
// results, warnings, errors, hints, prompts — goes through here and shares one
// shape: a `yolo:` prefix whose color encodes the status, followed by the
// plain-colored message. Status always goes to stderr; stdout is reserved for
// the data a command was asked for (reviews, listings, diffs).
//
// See "Output and status reporting" in docs/cli.md for the level table.

use colored::{ColoredString, Colorize};
use std::fmt::Display;
use std::io::Write;

/// Status levels, ordered roughly by severity. The level picks the color of
/// the `yolo:` prefix — nothing else about the line changes.
#[derive(Clone, Copy)]
enum Level {
    /// Progress / a state change underway (cyan).
    Info,
    /// A completed state change (green).
    Success,
    /// A non-fatal problem or something needing attention (yellow).
    Warn,
    /// A fatal failure; the command exits non-zero (red).
    Error,
    /// De-emphasized guidance or a no-op (dimmed).
    Hint,
}

fn prefix(level: Level) -> ColoredString {
    let p = "yolo:";
    match level {
        Level::Info => p.cyan(),
        Level::Success => p.green(),
        Level::Warn => p.yellow(),
        Level::Error => p.red(),
        Level::Hint => p.dimmed(),
    }
}

fn render(level: Level, msg: impl Display) -> String {
    format!("{} {msg}", prefix(level))
}

/// Progress or a state change underway: `yolo:` cyan.
pub fn info(msg: impl Display) {
    eprintln!("{}", render(Level::Info, msg));
}

/// A completed state change: `yolo:` green.
pub fn success(msg: impl Display) {
    eprintln!("{}", render(Level::Success, msg));
}

/// A non-fatal problem or something needing attention: `yolo:` yellow.
pub fn warn(msg: impl Display) {
    eprintln!("{}", render(Level::Warn, msg));
}

/// A fatal failure — the command is about to exit non-zero: `yolo:` red.
pub fn error(msg: impl Display) {
    eprintln!("{}", render(Level::Error, msg));
}

/// De-emphasized guidance or a no-op: `yolo:` dimmed.
pub fn hint(msg: impl Display) {
    eprintln!("{}", render(Level::Hint, msg));
}

/// An interactive question: `yolo:` yellow, no trailing newline, flushed so
/// the cursor waits at the end of the question.
pub fn prompt(msg: impl Display) {
    eprint!("{} ", render(Level::Warn, msg));
    std::io::stderr().flush().ok();
}

/// A continuation line under the preceding status line — indented, uncolored,
/// no prefix (blocking PIDs, the `rule:` line under an ask, `→ allow`).
pub fn detail(msg: impl Display) {
    eprintln!("  {msg}");
}

/// The empty *data* answer — when a data command has nothing to show, stdout
/// gets one dimmed parenthesized line in place of the rows: `(no changes
/// staged)`, `(no snapshots)`, `(no rules configured)`. Unlike the levels
/// above this is data ("the list is empty"), not status, so it goes to stdout.
pub fn empty(msg: impl Display) {
    println!("{}", render_empty(msg));
}

fn render_empty(msg: impl Display) -> String {
    format!("({msg})").dimmed().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, not three: the colored override is process-global and the
    /// default runner is parallel, so the color-on and color-off phases must
    /// run sequentially.
    #[test]
    fn render_shape_and_level_colors() {
        // Color off: every level renders the same plain `yolo: <msg>` line.
        colored::control::set_override(false);
        for level in [
            Level::Info,
            Level::Success,
            Level::Warn,
            Level::Error,
            Level::Hint,
        ] {
            assert_eq!(render(level, "hello"), "yolo: hello");
        }
        assert_eq!(render_empty("no changes"), "(no changes)");

        // Color on: each level starts with its documented color (docs/cli.md),
        // and the escape codes wrap only the `yolo:` prefix — the message body
        // stays outside every SGR sequence.
        colored::control::set_override(true);
        for (level, code) in [
            (Level::Info, "\x1b[36m"),    // cyan
            (Level::Success, "\x1b[32m"), // green
            (Level::Warn, "\x1b[33m"),    // yellow
            (Level::Error, "\x1b[31m"),   // red
            (Level::Hint, "\x1b[2m"),     // dimmed
        ] {
            let line = render(level, "boom");
            assert!(
                line.starts_with(code),
                "expected {line:?} to start with {code:?}"
            );
            let reset = "\x1b[0m";
            let end_of_prefix = line.find(reset).expect("prefix ends with a reset") + reset.len();
            assert_eq!(&line[end_of_prefix..], " boom", "body must be uncolored");
            assert!(line[..end_of_prefix].contains("yolo:"));
        }
        // The empty-data line is dimmed as a whole — parens and all.
        assert!(render_empty("no changes").starts_with("\x1b[2m(no changes)"));
        colored::control::unset_override();
    }
}
