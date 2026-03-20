// agfs CLI — audit.rs
//
// `agfs audit`                — full session history (every record, dead branches dimmed).
// `agfs audit --path <path>`  — trace operations on a specific file.

use crate::journal;
use crate::journal::{Marker, SegmentedJournal};
use anyhow::Result;
use colored::Colorize;

/// Display the full journal with dead branches dimmed.
pub fn run(path_filter: Option<&str>) -> Result<()> {
    let agfs = crate::utils::session_dir()?;
    let path_filter = path_filter.map(crate::utils::normalize_path);

    let sj = SegmentedJournal::new(journal::read(&agfs)?);

    if sj.segments.iter().all(|s| s.records.is_empty()) && sj.markers.is_empty() {
        println!("{}", "No journal records.".yellow());
        return Ok(());
    }

    let alive = sj.markers.alive_segments(sj.segments.len());

    for (seg_idx, segment) in sj.segments.iter().enumerate() {
        let reachable = alive[seg_idx];

        for record in &segment.records {
            if let Some(filter) = path_filter.as_deref()
                && !record_matches_path(record, filter)
            {
                continue;
            }

            let line = format_record(record);
            if reachable {
                println!("  {line}");
            } else {
                println!("  {} {}", line.dimmed(), "(unreachable)".dimmed());
            }
        }

        // Print the marker after this segment (if any).
        if let Some(marker) = sj.markers.get(seg_idx) {
            let marker_record = marker.to_record();
            if path_filter.is_none() || is_structural(&marker_record) {
                let line = format_record(&marker_record);
                if reachable {
                    println!("  {line}");
                } else {
                    println!("  {} {}", line.dimmed(), "(unreachable)".dimmed());
                }
            }
        }
    }

    Ok(())
}

fn is_structural(record: &journal::Record) -> bool {
    matches!(
        record,
        journal::Record::Checkpoint(_) | journal::Record::Restore { .. }
    )
}

fn record_matches_path(record: &journal::Record, filter: &str) -> bool {
    match record {
        journal::Record::Added { path, .. }
        | journal::Record::Modified { path, .. }
        | journal::Record::Deleted { path } => path == filter,
        journal::Record::Redirect { old, new, .. } | journal::Record::Replace { old, new, .. } => {
            old == filter || new == filter
        }
        _ => false,
    }
}

fn format_record(record: &journal::Record) -> String {
    match record {
        journal::Record::Checkpoint(c) => {
            format!("{} {}", format!("[{}]", c.gen_id).cyan().bold(), c.name)
        }
        journal::Record::Restore {
            gen_id, target_gen, ..
        } => {
            format!(
                "{} restored to [{}]",
                format!("[{gen_id}]").yellow().bold(),
                target_gen,
            )
        }
        journal::Record::Added { path, ino, .. } => {
            format!("{:10} {}  (ino {})", "added".green(), path, ino)
        }
        journal::Record::Modified { path, ino, .. } => {
            format!("{:10} {}  (ino {})", "modified".blue(), path, ino)
        }
        journal::Record::Deleted { path } => {
            format!("{:10} {}", "deleted".red(), path)
        }
        journal::Record::Redirect { old, new, .. } => {
            format!("{:10} {} → {}", "renamed".magenta(), old, new)
        }
        journal::Record::Replace { old, new, .. } => {
            format!("{:10} {} → {}", "replaced".magenta(), old, new)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Checkpoint, DType, Record};

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
        let rec = Record::Checkpoint(Checkpoint {
            gen_id: 3,
            name: "build".into(),
        });
        let s = strip_ansi(&format_record(&rec));
        assert!(s.contains("[3]"), "should contain gen_id: {s}");
        assert!(s.contains("build"), "should contain name: {s}");
    }

    #[test]
    fn format_restore() {
        let rec = Record::Restore {
            gen_id: 5,
            target_gen: 2,
        };
        let s = strip_ansi(&format_record(&rec));
        assert!(s.contains("[5]"), "should contain gen_id: {s}");
        assert!(
            s.contains("restored to [2]"),
            "should reference target: {s}"
        );
    }

    #[test]
    fn format_added() {
        let rec = Record::Added {
            path: "/src/main.rs".into(),
            dtype: Some(DType::File),
            ino: 42,
        };
        let s = strip_ansi(&format_record(&rec));
        assert!(s.contains("added"), "should say added: {s}");
        assert!(s.contains("/src/main.rs"), "should contain path: {s}");
        assert!(s.contains("42"), "should contain ino: {s}");
    }

    #[test]
    fn format_replace() {
        let rec = Record::Replace {
            old: "/a".into(),
            new: "/b".into(),
            dtype: Some(DType::File),
        };
        let s = strip_ansi(&format_record(&rec));
        assert!(s.contains("replaced"), "should say replaced: {s}");
        assert!(s.contains("/a"), "should contain old: {s}");
        assert!(s.contains("/b"), "should contain new: {s}");
    }
}
