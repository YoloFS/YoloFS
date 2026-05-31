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

    if journal.segments.iter().all(|s| s.records.is_empty()) && journal.markers.len() <= 1 {
        println!("{}", "No journal records.".yellow());
        return Ok(());
    }

    for (seg_idx, segment) in journal.segments.iter().enumerate() {
        let reachable = journal.is_alive(seg_idx);

        for record in &segment.records {
            let line = match record {
                journal::Record::Action(action) => {
                    if let Some(filter) = path_filter.as_deref()
                        && !action_matches_path(action, filter)
                    {
                        continue;
                    }
                    format_action(action)
                }
                journal::Record::Note(note) => {
                    if let Some(filter) = path_filter.as_deref()
                        && !note_matches_path(note, filter)
                    {
                        continue;
                    }
                    format_note(note)
                }
                // Markers never appear inside a segment (they split segments).
                journal::Record::Marker(_) => continue,
            };
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
        journal::Action::Stage { path, .. } | journal::Action::Delete { path, .. } => {
            path == filter
        }
        journal::Action::Rename { dst, src, .. } => src == filter || dst == filter,
    }
}

fn note_matches_path(note: &journal::Note, filter: &str) -> bool {
    match note {
        journal::Note::Block { path } => path == filter,
    }
}

fn format_marker(marker: &journal::Marker) -> String {
    match marker {
        journal::Marker::Snapshot { gen_id, name } => {
            format!("{} {}", format!("[{}]", gen_id).cyan().bold(), name)
        }
        journal::Marker::Travel {
            gen_id, target_gen, ..
        } => {
            format!(
                "{} traveled to [{}]",
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

fn format_note(note: &journal::Note) -> String {
    match note {
        journal::Note::Block { path } => {
            format!("{:10} {}", "blocked".yellow(), path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Action, Marker, Note};

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
    fn format_snapshot() {
        let marker = Marker::Snapshot {
            gen_id: 3,
            name: "build".into(),
        };
        let s = strip_ansi(&format_marker(&marker));
        assert!(s.contains("[3]"), "should contain gen_id: {s}");
        assert!(s.contains("build"), "should contain name: {s}");
    }

    #[test]
    fn format_travel() {
        let marker = Marker::Travel {
            gen_id: 5,
            target_gen: 2,
        };
        let s = strip_ansi(&format_marker(&marker));
        assert!(s.contains("[5]"), "should contain gen_id: {s}");
        assert!(
            s.contains("traveled to [2]"),
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

    #[test]
    fn format_blocked() {
        let note = Note::Block {
            path: "/etc/passwd".into(),
        };
        let s = strip_ansi(&format_note(&note));
        assert!(s.contains("blocked"), "should say blocked: {s}");
        assert!(s.contains("/etc/passwd"), "should contain path: {s}");
    }

    #[test]
    fn note_path_filter_matches() {
        let note = Note::Block {
            path: "/etc/passwd".into(),
        };
        assert!(note_matches_path(&note, "/etc/passwd"));
        assert!(!note_matches_path(&note, "/etc/shadow"));
    }
}
