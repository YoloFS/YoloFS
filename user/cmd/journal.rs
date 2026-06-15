// yolo CLI — journal.rs
//
// `yolo journal`                  — the raw record log over a range (every op +
//                                   access note, dead branches dimmed).
// `yolo journal [<id>|a..b|all]`  — scope to a snapshot / range (review's grammar).
// `yolo journal -- <path>`        — trace operations on a specific file.
//
// The curated net-change view is `yolo review`; this is the raw underside.

use crate::journal::{self, Journal};
use anyhow::Result;
use colored::Colorize;

/// Display the raw journal records with dead branches dimmed, over the same
/// `[<id>|a..b|all]` range grammar as `review` (default: the latest snapshot).
pub fn run(range: Option<&str>, path: Option<&str>) -> Result<()> {
    let yolofs = crate::utils::session_dir()?;
    let path_filter = path.map(crate::utils::normalize_path);

    let journal = Journal::read(&yolofs)?;

    if journal.segments.iter().all(|s| s.records.is_empty()) && journal.markers.len() <= 1 {
        crate::report::empty("no journal records");
        return Ok(());
    }

    let (start, end) = super::review::parse_range(range, false, &journal)?;

    for seg_idx in start..end {
        let segment = &journal.segments[seg_idx];
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
            let line = format_marker(seg_idx + 1, marker);
            if reachable {
                println!("  {line}");
            } else {
                println!("  {} {}", line.dimmed(), "(unreachable)".dimmed());
            }
        }
    }

    // In the default (latest) view, point at the full log when older history is
    // hidden (`start > 0`).
    if range.is_none() && start > 0 {
        println!(
            "{}",
            "(latest snapshot · `yolo journal all` for the full log)".dimmed()
        );
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
        journal::Note::Ask { path, .. } | journal::Note::Block { path, .. } => path == filter,
    }
}

fn format_marker(gen_id: usize, marker: &journal::Marker) -> String {
    match marker {
        journal::Marker::Snapshot { name } => {
            format!("{} {}", format!("snapshot {gen_id}").cyan().bold(), name)
        }
        journal::Marker::Travel { target_gen } => {
            format!(
                "{} → {}",
                format!("travel {gen_id}").yellow().bold(),
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
            format!("{:10} {} → {}", "renamed".cyan(), src, dst)
        }
    }
}

fn format_note(note: &journal::Note) -> String {
    match note {
        journal::Note::Ask { path, op, decision } => {
            format!(
                "{:10} {:5} {} → {}",
                "ask".yellow(),
                op.label(),
                path,
                decision
            )
        }
        journal::Note::Block {
            path,
            op,
            rule_path,
        } => {
            if rule_path.is_empty() {
                format!("{:10} {:5} {}", "blocked".yellow(), op.label(), path)
            } else {
                format!(
                    "{:10} {:5} {} by {}",
                    "blocked".yellow(),
                    op.label(),
                    path,
                    rule_path
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Action, Marker, Note, Op};

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
            name: "build".into(),
        };
        let s = strip_ansi(&format_marker(3, &marker));
        assert!(s.contains("snapshot 3"), "should contain gen: {s}");
        assert!(s.contains("build"), "should contain name: {s}");
    }

    #[test]
    fn format_travel() {
        let marker = Marker::Travel { target_gen: 2 };
        let s = strip_ansi(&format_marker(5, &marker));
        assert!(s.contains("travel 5"), "should contain gen: {s}");
        assert!(s.contains("→ 2"), "should reference target: {s}");
    }

    #[test]
    fn format_staged() {
        let action = Action::Stage {
            path: "/src/main.rs".into(),
            ino: 42,
            pre: journal::Target::Absence,
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
            src_pre: journal::Target::BasePath("/a".into()),
            dst_pre: journal::Target::Absence,
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
            op: Op::Write,
            rule_path: "/etc".into(),
        };
        let s = strip_ansi(&format_note(&note));
        assert!(s.contains("blocked"), "should say blocked: {s}");
        assert!(s.contains("/etc/passwd"), "should contain path: {s}");
        assert!(s.contains("/etc"), "should contain rule path: {s}");
    }

    #[test]
    fn format_blocked_empty_rule_path() {
        // The rare unresolvable-rule case: render cleanly, no dangling " by ".
        let note = Note::Block {
            path: "/etc/passwd".into(),
            op: Op::Write,
            rule_path: String::new(),
        };
        let s = strip_ansi(&format_note(&note));
        assert!(s.contains("blocked"), "should say blocked: {s}");
        assert!(s.contains("/etc/passwd"), "should contain path: {s}");
        assert!(
            !s.contains(" by "),
            "no dangling 'by' for empty rule_path: {s}"
        );
    }

    #[test]
    fn note_path_filter_matches() {
        let note = Note::Block {
            path: "/etc/passwd".into(),
            op: Op::Write,
            rule_path: "/etc".into(),
        };
        assert!(note_matches_path(&note, "/etc/passwd"));
        assert!(!note_matches_path(&note, "/etc/shadow"));
    }
}
