use super::helpers::{ino_for, inode_path, inos, journal, markers, tree};
use crate::helpers::YoloSession;
use std::fs;
use std::os::unix::fs::MetadataExt;
use yolofs::journal::Marker;

// ── Journal state after travel ──────────────────────────────────────────

/// Travel keeps journal records up to and including the snapshot marker.
#[test]
fn travel_journal_contains_snapshot_marker() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["snapshot", "chk1"]).expect("snapshot");
    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");

    s.cli(&["travel", "1"]).expect("travel");

    let j = journal(&s);
    let mkrs = markers(&j);
    assert!(
        mkrs.iter()
            .any(|m| matches!(m, Marker::Snapshot { name, .. } if name == "chk1")),
        "chk1 marker should be in journal: {mkrs:?}"
    );
}

/// Travel appends a J record; reachable + resolve excludes post-snapshot mutations.
#[test]
fn travel_journal_has_no_post_snapshot_records() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["snapshot", "chk1"]).expect("snapshot");
    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");
    fs::remove_file(s.mnt_path("a.txt")).expect("rm a");

    s.cli(&["travel", "1"]).expect("travel");

    let j = journal(&s);
    let mkrs = markers(&j);

    // J (Travel) record should be present in the raw journal.
    assert!(
        mkrs.iter().any(|m| matches!(m, Marker::Travel { .. })),
        "Travel record should be in journal: {mkrs:?}"
    );

    // reachable + resolve should match the snapshot state (only a.txt).
    let t = j.into_tree();
    let debug = format!("{t:?}");
    assert!(
        debug.contains("a.txt"),
        "a.txt should be in live dentries: {debug}"
    );
    assert!(
        !debug.contains("b.txt"),
        "b.txt should NOT be in live dentries: {debug}"
    );
}

// ── Inode store after travel ────────────────────────────────────────────

/// Pre-snapshot inodes are preserved after travel.
#[test]
fn travel_keeps_pre_snapshot_inodes() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    let pre_ino = ino_for(&tree(&s), "/a.txt");
    s.cli(&["snapshot", "chk1"]).expect("snapshot");

    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");
    s.cli(&["travel", "1"]).expect("travel");

    assert!(
        inode_path(&s, pre_ino).exists(),
        "pre-snapshot inode {pre_ino} should still exist on disk"
    );
    assert_eq!(
        fs::read_to_string(inode_path(&s, pre_ino)).unwrap(),
        "a\n",
        "pre-snapshot inode content should be intact"
    );
}

/// Post-snapshot inodes are orphaned but still on disk after travel.
#[test]
fn travel_orphans_post_snapshot_inodes() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["snapshot", "chk1"]).expect("snapshot");

    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");
    let post_ino = ino_for(&tree(&s), "/b.txt");

    s.cli(&["travel", "1"]).expect("travel");

    // Inode file still on disk (orphaned)
    assert!(
        inode_path(&s, post_ino).exists(),
        "orphaned inode should still be on disk"
    );

    // But not referenced by any resolved change
    let t = tree(&s);
    assert!(
        !t.any(|_, d| d.ino() == Some(post_ino)),
        "orphaned inode should not appear in resolved dentries"
    );
}

/// Abort after travel cleans up all inodes including orphans.
#[test]
fn abort_after_travel_cleans_orphans() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["snapshot", "chk1"]).expect("snapshot");
    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");

    s.cli(&["travel", "1"]).expect("travel");
    s.cli(&["abort", "--force"]).expect("abort");

    assert!(
        inos(&s).is_empty(),
        "abort should clear all inodes including orphans"
    );
}

/// After a travel, newly created files must receive fresh (monotonically
/// increasing) inode numbers — orphaned post-snapshot inodes must never
/// be recycled.
#[test]
fn travel_new_files_get_fresh_inodes() {
    let s = YoloSession::new().expect("session setup");

    // Create a file and snapshot.
    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["snapshot", "chk1"]).expect("snapshot");

    // Post-snapshot: create another file.
    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");
    let inos_before = inos(&s);
    let max_ino_before = *inos_before.last().expect("should have inodes");

    // Travel — b.txt's inode becomes orphaned.
    s.cli(&["travel", "1"]).expect("travel");

    // Create new files after travel.
    fs::write(s.mnt_path("c.txt"), "c\n").expect("write c");
    fs::write(s.mnt_path("d.txt"), "d\n").expect("write d");

    // Identify inodes allocated after travel.
    let inos_after = inos(&s);
    let fresh: Vec<u32> = inos_after
        .iter()
        .copied()
        .filter(|ino| !inos_before.contains(ino))
        .collect();

    // Must have at least 2 new inodes (for c.txt and d.txt).
    assert!(
        fresh.len() >= 2,
        "expected at least 2 fresh inodes, got {fresh:?}"
    );

    // Every new inode must be strictly greater than any pre-travel inode.
    for ino in &fresh {
        assert!(
            *ino > max_ino_before,
            "inode {ino} should be > {max_ino_before} (inodes must not be recycled)"
        );
    }
}

// ── Re-COW behavior after travel ────────────────────────────────────────

/// Writing to a traveled file without a new snapshot reuses the inode.
#[test]
fn write_after_travel_without_snapshot_reuses_inode() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("file.txt"), "v1\n").expect("write v1");
    s.cli(&["snapshot", "chk1"]).expect("snapshot");

    fs::write(s.mnt_path("file.txt"), "v2\n").expect("write v2");
    s.cli(&["travel", "1"]).expect("travel");

    // snapshot_gen is now set to chk1's gen, and the dirent's
    // snapshot_gen matches. So writing should open the inode directly
    // (truncate in place), not allocate a new one.
    let inos_before = inos(&s);
    fs::write(s.mnt_path("file.txt"), "v1-modified\n").expect("write after travel");
    let inos_after = inos(&s);

    assert_eq!(
        inos_before.len(),
        inos_after.len(),
        "no new inode should be allocated: before={inos_before:?}, after={inos_after:?}"
    );
}

/// Writing after travel + new snapshot triggers re-COW (new inode).
#[test]
fn write_after_travel_and_snapshot_triggers_recow() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("file.txt"), "v1\n").expect("write v1");
    s.cli(&["snapshot", "chk1"]).expect("snapshot");
    fs::write(s.mnt_path("file.txt"), "v2\n").expect("write v2");

    s.cli(&["travel", "1"]).expect("travel");
    s.cli(&["snapshot", "post-travel"]).expect("new snapshot");

    let inos_before = inos(&s);
    fs::write(s.mnt_path("file.txt"), "v3\n").expect("write triggers re-COW");
    let inos_after = inos(&s);

    assert_eq!(
        inos_after.len(),
        inos_before.len() + 1,
        "re-COW should allocate new inode: before={inos_before:?}, after={inos_after:?}"
    );
    assert_eq!(fs::read_to_string(s.mnt_path("file.txt")).unwrap(), "v3\n");
}

/// Re-COW after travel preserves the pre-snapshot inode content.
#[test]
fn recow_after_travel_preserves_old_inode() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("file.txt"), "v1\n").expect("write v1");
    let v1_ino = ino_for(&tree(&s), "/file.txt");
    s.cli(&["snapshot", "chk1"]).expect("snapshot");

    fs::write(s.mnt_path("file.txt"), "v2\n").expect("write v2 (re-COW)");

    s.cli(&["travel", "1"]).expect("travel");
    s.cli(&["snapshot", "post-travel"]).expect("new snapshot");

    fs::write(s.mnt_path("file.txt"), "v3\n").expect("write v3 (re-COW)");

    // The v1 inode should still have the original content
    assert_eq!(
        fs::read_to_string(inode_path(&s, v1_ino)).unwrap(),
        "v1\n",
        "original inode should be untouched after re-COW"
    );
}

// ── Resolved state correctness after travel ─────────────────────────────

/// Resolved dentries after travel exactly match the snapshot state.
#[test]
fn resolved_changes_match_snapshot_state() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");
    s.cli(&["snapshot", "chk1"]).expect("snapshot");

    fs::write(s.mnt_path("c.txt"), "c\n").expect("write c");
    fs::remove_file(s.mnt_path("a.txt")).expect("delete a");

    s.cli(&["travel", "1"]).expect("travel");

    let t = tree(&s);
    assert_eq!(t.len(), 2, "exactly 2 dentries: {t:?}");

    let debug = format!("{t:?}");
    assert!(
        debug.contains("a.txt"),
        "a.txt should be in dentries: {debug}"
    );
    assert!(
        debug.contains("b.txt"),
        "b.txt should be in dentries: {debug}"
    );
    assert!(
        !debug.contains("c.txt"),
        "c.txt should NOT be in dentries: {debug}"
    );
}

// ── Renamed d_type correctness after travel ─────────────────────────────

/// Renamed directory resolves to a Renamed change after travel.
#[test]
fn travel_renamed_directory_in_resolved_changes() {
    let s = YoloSession::new().expect("session setup");

    // Create directory in base first via commit
    fs::create_dir(s.mnt_path("old_dir")).expect("mkdir");
    fs::write(s.mnt_path("old_dir/inner.txt"), "inner\n").expect("write");
    s.cli(&["commit"]).expect("commit to base");

    // Now rename the base directory through the mount
    fs::rename(s.mnt_path("old_dir"), s.mnt_path("new_dir")).expect("rename dir");
    s.cli(&["snapshot", "chk1"]).expect("snapshot");

    fs::write(s.mnt_path("extra.txt"), "extra\n").expect("write post");

    s.cli(&["travel", "1"]).expect("travel");

    let t = tree(&s);
    let debug = format!("{t:?}");

    // The rename should survive travel
    assert!(
        debug.contains("new_dir"),
        "new_dir should be in dentries: {debug}"
    );
    assert!(
        !debug.contains("extra.txt"),
        "post-snapshot file should NOT be in dentries: {debug}"
    );

    // Verify the directory is accessible and d_type is dir via symlink_metadata
    let md = fs::symlink_metadata(s.mnt_path("new_dir")).expect("lstat new_dir");
    assert!(
        md.file_type().is_dir(),
        "new_dir should be a directory after travel"
    );
    assert_eq!(
        fs::read_to_string(s.mnt_path("new_dir/inner.txt")).unwrap(),
        "inner\n"
    );
}

/// Renamed symlink resolves correctly after travel and inode store is consistent.
#[test]
fn travel_renamed_symlink_in_resolved_changes() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("target.txt"), "target\n").expect("write target");
    std::os::unix::fs::symlink("target.txt", s.mnt_path("old_link")).expect("symlink");
    fs::rename(s.mnt_path("old_link"), s.mnt_path("new_link")).expect("rename symlink");
    s.cli(&["snapshot", "chk1"]).expect("snapshot");

    fs::write(s.mnt_path("post.txt"), "post\n").expect("write post");

    s.cli(&["travel", "1"]).expect("travel");

    let t = tree(&s);
    let debug = format!("{t:?}");

    assert!(
        debug.contains("new_link"),
        "new_link should be in dentries: {debug}"
    );
    assert!(
        !debug.contains("post.txt"),
        "post-snapshot file should NOT be in dentries: {debug}"
    );

    // Verify d_type is symlink via lstat through the mount
    let md = fs::symlink_metadata(s.mnt_path("new_link")).expect("lstat new_link");
    assert!(
        md.file_type().is_symlink(),
        "new_link should be a symlink after travel"
    );

    // Journal should have the snapshot marker and records up to it
    let j = journal(&s);
    let mkrs = markers(&j);
    assert!(
        mkrs.iter()
            .any(|m| matches!(m, Marker::Snapshot { name, .. } if name == "chk1")),
        "chk1 should be in journal: {mkrs:?}"
    );
}

// ── Journal inode and byte-level invariants ──────────────────────────────

/// The journal file inode must be preserved across travel (set_len, not replace).
#[test]
fn travel_preserves_journal_inode() {
    let s = YoloSession::new().expect("session setup");

    let journal_path = s.root.join(".yolofs/journal");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["snapshot", "chk1"]).expect("snapshot");
    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");

    let ino_before = fs::metadata(&journal_path).expect("stat before").ino();

    s.cli(&["travel", "1"]).expect("travel");

    let ino_after = fs::metadata(&journal_path).expect("stat after").ino();
    assert_eq!(
        ino_before, ino_after,
        "journal inode must be preserved across travel"
    );
}

/// After travel, the journal grows (J record appended) and original bytes are preserved.
#[test]
fn travel_journal_is_byte_prefix() {
    let s = YoloSession::new().expect("session setup");

    let journal_path = s.root.join(".yolofs/journal");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["snapshot", "chk1"]).expect("snapshot");
    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");

    let bytes_before = fs::read(&journal_path).expect("read before");

    s.cli(&["travel", "1"]).expect("travel");

    let bytes_after = fs::read(&journal_path).expect("read after");
    assert!(
        bytes_after.len() > bytes_before.len(),
        "journal should grow after travel (J record appended): before={} after={}",
        bytes_before.len(),
        bytes_after.len()
    );
    assert_eq!(
        &bytes_after[..bytes_before.len()],
        &bytes_before[..],
        "original journal bytes must be preserved as a prefix"
    );

    // Verify the J record is present.
    let j = journal(&s);
    let mkrs = markers(&j);
    assert!(
        mkrs.iter().any(|m| matches!(m, Marker::Travel { .. })),
        "Travel record should be in journal: {mkrs:?}"
    );
}

/// The J record written by travel should have a gen_id higher than the
/// target snapshot's gen_id (monotonically increasing).
#[test]
fn travel_j_record_has_correct_gen() {
    let s = YoloSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["snapshot", "chk1"]).expect("snapshot");

    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");
    s.cli(&["snapshot", "chk2"]).expect("snapshot 2");

    s.cli(&["travel", "1"]).expect("travel");

    let j = journal(&s);
    let mkrs = markers(&j);

    // Find the snapshot gens (= marker index) and the travel record.
    let chk1_gen = mkrs
        .iter()
        .enumerate()
        .find_map(|(gen_id, m)| match m {
            Marker::Snapshot { name } if name == "chk1" => Some(gen_id),
            _ => None,
        })
        .expect("chk1 should exist");

    let chk2_gen = mkrs
        .iter()
        .enumerate()
        .find_map(|(gen_id, m)| match m {
            Marker::Snapshot { name } if name == "chk2" => Some(gen_id),
            _ => None,
        })
        .expect("chk2 should exist");

    let (s_gen, s_target) = mkrs
        .iter()
        .enumerate()
        .find_map(|(gen_id, m)| match m {
            Marker::Travel { target_gen } => Some((gen_id, *target_gen as usize)),
            _ => None,
        })
        .expect("travel record should exist");

    assert_eq!(s_target, chk1_gen, "J record should target chk1");
    assert!(
        s_gen > chk2_gen,
        "J record gen ({s_gen}) should be greater than chk2 gen ({chk2_gen})"
    );
}
