// agfs CLI — status.rs
//
// `agfs status` — show staged changes (staging walk per §3.6).

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

/// A classified change entry.
#[derive(Debug)]
pub enum Change {
    Added(String),
    Modified(String),
    Deleted(String),
    Renamed { from: String, to: String },
    RenamedModified { from: String, to: String },
}

/// Read the renames file: sequence of old_path\0new_path\0 pairs.
fn read_renames(agfs_dir: &Path) -> Result<Vec<(String, String)>> {
    let renames_path = agfs_dir.join("renames");
    if !renames_path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read(&renames_path).context("reading renames file")?;
    let mut result = Vec::new();
    let mut parts: Vec<&[u8]> = data.split(|&b| b == 0).collect();
    // Remove trailing empty entry if present
    if parts.last().is_some_and(|p| p.is_empty()) {
        parts.pop();
    }
    for pair in parts.chunks(2) {
        if pair.len() == 2 {
            let old = String::from_utf8_lossy(pair[0]).to_string();
            let new = String::from_utf8_lossy(pair[1]).to_string();
            result.push((old, new));
        }
    }
    Ok(result)
}

/// Check if a path is a whiteout (char device 0/0).
fn is_whiteout(path: &Path) -> bool {
    if let Ok(meta) = fs::symlink_metadata(path) {
        meta.file_type().is_char_device() && {
            use std::os::linux::fs::MetadataExt;
            meta.st_rdev() == 0
        }
    } else {
        false
    }
}

/// Recursively walk the staging directory and collect relative paths.
fn walk_staging(staging_dir: &Path, prefix: &str) -> Result<Vec<(String, PathBuf)>> {
    let mut entries = Vec::new();
    if !staging_dir.exists() {
        return Ok(entries);
    }
    for entry in fs::read_dir(staging_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let relpath = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let full = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            entries.extend(walk_staging(&full, &relpath)?);
        } else {
            entries.push((relpath, full));
        }
    }
    Ok(entries)
}

pub fn staging_walk(agfs_dir: &Path) -> Result<Vec<Change>> {
    let staging_dir = agfs_dir.join("staging");
    let session_root = agfs_dir.parent().unwrap_or(Path::new("."));
    let _ = &session_root; // used in future for relative path resolution
    let base = Path::new("/");

    // Step 1: Read renames
    let renames = read_renames(agfs_dir)?;
    let rename_old_set: HashMap<String, String> = renames
        .iter()
        .map(|(old, new)| (old.clone(), new.clone()))
        .collect();
    let rename_new_set: HashMap<String, String> = renames
        .iter()
        .map(|(old, new)| (new.clone(), old.clone()))
        .collect();

    // Step 2: Walk staging
    let staging_entries = walk_staging(&staging_dir, "")?;

    let mut changes = Vec::new();
    let mut consumed_renames: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Step 3: Process rename records
    for (old_path, new_path) in &renames {
        let staging_new = staging_dir.join(new_path.trim_start_matches('/'));
        if staging_new.exists() && !is_whiteout(&staging_new) {
            changes.push(Change::RenamedModified {
                from: old_path.clone(),
                to: new_path.clone(),
            });
            consumed_renames.insert(new_path.trim_start_matches('/').to_string());
        } else {
            changes.push(Change::Renamed {
                from: old_path.clone(),
                to: new_path.clone(),
            });
        }
        consumed_renames.insert(old_path.trim_start_matches('/').to_string());
    }

    // Step 4: Classify remaining entries
    for (relpath, full_path) in staging_entries {
        let normalized = relpath.trim_start_matches('/').to_string();
        // Skip entries already explained by renames
        if consumed_renames.contains(&normalized) {
            continue;
        }
        // Also skip whiteouts at old_path of a rename
        if rename_old_set.contains_key(&relpath)
            || rename_old_set.contains_key(&format!("/{relpath}"))
        {
            continue;
        }
        // Skip staged files at new_path of a rename
        if rename_new_set.contains_key(&relpath)
            || rename_new_set.contains_key(&format!("/{relpath}"))
        {
            continue;
        }

        if is_whiteout(&full_path) {
            changes.push(Change::Deleted(relpath));
        } else {
            // Check if file exists in base
            let base_file = base.join(&relpath);
            if base_file.exists() {
                changes.push(Change::Modified(relpath));
            } else {
                changes.push(Change::Added(relpath));
            }
        }
    }

    Ok(changes)
}

use colored::Colorize;

pub fn run() -> Result<()> {
    let agfs = crate::ctl::agfs_dir()?;
    if !agfs.exists() {
        anyhow::bail!("no agfs session found (no .agfs/ directory)");
    }

    let changes = staging_walk(&agfs)?;

    if changes.is_empty() {
        println!("{}", "No changes staged.".yellow());
        return Ok(());
    }

    for change in &changes {
        match change {
            Change::Added(p) => println!("  {} {}", p, "(added)".green()),
            Change::Modified(p) => println!("  {} {}", p, "(modified)".yellow()),
            Change::Deleted(p) => println!("  {} {}", p, "(deleted)".red()),
            Change::Renamed { from, to } => {
                println!("  {} → {} {}", from, to, "(renamed)".cyan())
            }
            Change::RenamedModified { from, to } => {
                println!("  {} → {} {}", from, to, "(renamed + modified)".cyan())
            }
        }
    }

    let n = changes.len();
    println!(
        "\n{}",
        format!("{n} staged change{}", if n == 1 { "" } else { "s" }).bold()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_test_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let agfs = dir.path();
        fs::create_dir_all(agfs.join("staging")).unwrap();
        dir
    }

    #[test]
    fn empty_staging() {
        let dir = setup_test_dir();
        let changes = staging_walk(dir.path()).unwrap();
        assert!(changes.is_empty());
    }

    #[test]
    fn modified_file() {
        let dir = setup_test_dir();
        // Create a file in staging that also exists in base (/)
        let staging = dir.path().join("staging");
        fs::create_dir_all(staging.join("etc")).unwrap();
        fs::write(staging.join("etc/hostname"), "modified").unwrap();

        let changes = staging_walk(dir.path()).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], Change::Modified(p) if p.contains("hostname")));
    }

    #[test]
    fn added_file() {
        let dir = setup_test_dir();
        let staging = dir.path().join("staging");
        // Create a staging file with a path that does NOT exist in base
        fs::create_dir_all(staging.join("nonexistent_unique_dir_12345")).unwrap();
        fs::write(staging.join("nonexistent_unique_dir_12345/new.txt"), "new").unwrap();

        let changes = staging_walk(dir.path()).unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], Change::Added(p) if p.contains("new.txt")));
    }

    #[test]
    fn read_renames_empty() {
        let dir = setup_test_dir();
        let renames = read_renames(dir.path()).unwrap();
        assert!(renames.is_empty());
    }

    #[test]
    fn read_renames_file() {
        let dir = setup_test_dir();
        // Write a renames file: old\0new\0
        let data = b"/old/path\0/new/path\0";
        fs::write(dir.path().join("renames"), data).unwrap();

        let renames = read_renames(dir.path()).unwrap();
        assert_eq!(renames.len(), 1);
        assert_eq!(renames[0], ("/old/path".to_string(), "/new/path".to_string()));
    }

    #[test]
    fn read_renames_multiple() {
        let dir = setup_test_dir();
        let data = b"/a\0/b\0/c\0/d\0";
        fs::write(dir.path().join("renames"), data).unwrap();

        let renames = read_renames(dir.path()).unwrap();
        assert_eq!(renames.len(), 2);
        assert_eq!(renames[0], ("/a".to_string(), "/b".to_string()));
        assert_eq!(renames[1], ("/c".to_string(), "/d".to_string()));
    }
}
