// yolo CLI — audit.rs
//
// `yolo audit`                — full session history (every record, dead branches dimmed).
// `yolo audit --path <path>`  — trace operations on a specific file.

use crate::journal::{self, Journal};
use anyhow::Result;
use colored::Colorize;

/// Display the full journal with dead branches dimmed.
pub fn run(path_filter: Option<&str>) -> Result<()> {
    let yolofs = crate::utils::session_dir()?;
    let path_filter = path_filter.map(crate::utils::normalize_path);

    let journal = Journal::read(&yolofs)?;

    if journal.segments.iter().all(|s| s.records.is_empty()) && journal.metas.len() <= 1 {
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

        // Print the meta after this segment (if any).
        if let Some(meta) = journal.metas.get(seg_idx + 1) {
            let line = format_meta(meta);
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
        journal::Action::Stage { path, .. } | journal::Action::Delete { path, .. } => {
            path == filter
        }
        journal::Action::Rename { dst, src, .. } => src == filter || dst == filter,
    }
}

fn format_meta(meta: &journal::Meta) -> String {
    match meta {
        journal::Meta::Mark { gen_id, name } => {
            format!("{} {}", format!("[{}]", gen_id).cyan().bold(), name)
        }
        journal::Meta::Jump {
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
        journal::Action::Stage { path, ino, .. } => {
            format!("{:10} {}  (ino {})", "staged".green(), path, ino)
        }
        journal::Action::Delete { path, .. } => {
            format!("{:10} {}", "deleted".red(), path)
        }
        journal::Action::Rename { src, dst, .. } => {
            format!("{:10} {} → {}", "renamed".magenta(), src, dst)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Action, Meta};

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
        let meta = Meta::Mark {
            gen_id: 3,
            name: "build".into(),
        };
        let s = strip_ansi(&format_meta(&meta));
        assert!(s.contains("[3]"), "should contain gen_id: {s}");
        assert!(s.contains("build"), "should contain name: {s}");
    }

    #[test]
    fn format_restore() {
        let meta = Meta::Jump {
            gen_id: 5,
            target_gen: 2,
        };
        let s = strip_ansi(&format_meta(&meta));
        assert!(s.contains("[5]"), "should contain gen_id: {s}");
        assert!(
            s.contains("restored to [2]"),
            "should reference target: {s}"
        );
    }

    #[test]
    fn format_staged() {
        let action = Action::Stage {
            path: "/src/main.rs".into(),
            ino: 42,
        };
        let s = strip_ansi(&format_action(&action));
        assert!(s.contains("staged"), "should say staged: {s}");
        assert!(s.contains("/src/main.rs"), "should contain path: {s}");
        assert!(s.contains("42"), "should contain ino: {s}");
    }

    #[test]
    fn format_rename() {
        let action = Action::Rename {
            src: "/a".into(),
            dst: "/b".into(),
        };
        let s = strip_ansi(&format_action(&action));
        assert!(s.contains("renamed"), "should say renamed: {s}");
        assert!(s.contains("/a"), "should contain old: {s}");
        assert!(s.contains("/b"), "should contain new: {s}");
    }
}
