// agfs CLI — audit.rs
//
// `agfs audit`                — full session history (every record, dead branches dimmed).
// `agfs audit --path <path>`  — trace operations on a specific file.

use crate::journal::{self, Journal};
use anyhow::Result;
use colored::Colorize;

/// Display the full journal with dead branches dimmed.
pub fn run(path_filter: Option<&str>) -> Result<()> {
    let agfs = crate::utils::session_dir()?;
    let path_filter = path_filter.map(crate::utils::normalize_path);

    let journal = Journal::read(&agfs)?;

    if journal.segments.iter().all(|s| s.records.is_empty()) && journal.markers.len() <= 1 {
        println!("{}", "No journal records.".yellow());
        return Ok(());
    }

    for (seg_idx, segment) in journal.segments.iter().enumerate() {
        let reachable = journal.is_alive(seg_idx);

        for action in &segment.records {
            if let Some(filter) = path_filter.as_deref()
                && !action_matches_path(action, filter)
            {
                continue;
            }

            let line = format_action(action);
            if reachable {
                println!("  {line}");
            } else {
                println!("  {} {}", line.dimmed(), "(unreachable)".dimmed());
            }
        }

        // Print the marker after this segment (if any).
        if let Some(marker) = journal.markers.get(seg_idx + 1) {
            let line = format_marker(marker);
            if reachable {
                println!("  {line}");
            } else {
                println!("  {} {}", line.dimmed(), "(unreachable)".dimmed());
            }
        }
    }

    Ok(())
}

fn action_matches_path(action: &journal::Action, filter: &str) -> bool {
    match action {
        journal::Action::Add { path, .. }
        | journal::Action::Modify { path, .. }
        | journal::Action::Delete { path, .. } => path == filter,
        journal::Action::Rename { dst, src, .. } | journal::Action::Replace { dst, src, .. } => {
            src == filter || dst == filter
        }
    }
}

fn format_marker(marker: &journal::Marker) -> String {
    match marker {
        journal::Marker::Checkpoint { gen_id, name } => {
            format!("{} {}", format!("[{}]", gen_id).cyan().bold(), name)
        }
        journal::Marker::Restore {
            gen_id, target_gen, ..
        } => {
            format!(
                "{} restored to [{}]",
                format!("[{gen_id}]").yellow().bold(),
                target_gen,
            )
        }
    }
}

fn format_action(action: &journal::Action) -> String {
    match action {
        journal::Action::Add { path, ino, .. } => {
            format!("{:10} {}  (ino {})", "added".green(), path, ino)
        }
        journal::Action::Modify { path, ino, .. } => {
            format!("{:10} {}  (ino {})", "modified".blue(), path, ino)
        }
        journal::Action::Delete { path, .. } => {
            format!("{:10} {}", "deleted".red(), path)
        }
        journal::Action::Rename { src, dst, .. } => {
            format!("{:10} {} → {}", "renamed".magenta(), src, dst)
        }
        journal::Action::Replace { src, dst, .. } => {
            format!("{:10} {} → {}", "replaced".magenta(), src, dst)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Action, Marker};

    /// Strip ANSI escape codes for assertion matching.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut in_escape = false;
        for c in s.chars() {
            if c == '\x1b' {
                in_escape = true;
            } else if in_escape {
                if c.is_ascii_alphabetic() {
                    in_escape = false;
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn format_checkpoint() {
        let marker = Marker::Checkpoint {
            gen_id: 3,
            name: "build".into(),
        };
        let s = strip_ansi(&format_marker(&marker));
        assert!(s.contains("[3]"), "should contain gen_id: {s}");
        assert!(s.contains("build"), "should contain name: {s}");
    }

    #[test]
    fn format_restore() {
        let marker = Marker::Restore {
            gen_id: 5,
            target_gen: 2,
        };
        let s = strip_ansi(&format_marker(&marker));
        assert!(s.contains("[5]"), "should contain gen_id: {s}");
        assert!(
            s.contains("restored to [2]"),
            "should reference target: {s}"
        );
    }

    #[test]
    fn format_added() {
        let action = Action::Add {
            path: "/src/main.rs".into(),
            dtype: Some(libc::DT_REG),
            ino: 42,
        };
        let s = strip_ansi(&format_action(&action));
        assert!(s.contains("added"), "should say added: {s}");
        assert!(s.contains("/src/main.rs"), "should contain path: {s}");
        assert!(s.contains("42"), "should contain ino: {s}");
    }

    #[test]
    fn format_replace() {
        let action = Action::Replace {
            src: "/a".into(),
            dst: "/b".into(),
            dtype: Some(libc::DT_REG),
        };
        let s = strip_ansi(&format_action(&action));
        assert!(s.contains("replaced"), "should say replaced: {s}");
        assert!(s.contains("/a"), "should contain old: {s}");
        assert!(s.contains("/b"), "should contain new: {s}");
    }
}
