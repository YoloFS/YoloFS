use super::helpers::{changes, ino_for, inode_path, inos, journal};
use crate::helpers::AgfsSession;
use agfs::journal::Record;
use agfs::journal;
use std::fs;
use std::os::unix::fs::MetadataExt;

// ── Journal state after restore ──────────────────────────────────────────

/// Restore keeps journal records up to and including the checkpoint marker.
#[test]
fn restore_journal_contains_checkpoint_marker() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");
    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");

    s.cli(&["restore", "chk1"]).expect("restore");

    let records = journal(&s);
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Checkpoint(c) if c.name == "chk1")),
        "chk1 marker should be in journal: {records:?}"
    );
}

/// Restore appends an S record; reachable + resolve excludes post-checkpoint mutations.
#[test]
fn restore_journal_has_no_post_checkpoint_records() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");
    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");
    fs::remove_file(s.mnt_path("a.txt")).expect("rm a");

    s.cli(&["restore", "chk1"]).expect("restore");

    let records = journal(&s);

    // S (Restore) record should be present in the raw journal.
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Restore { .. })),
        "Restore record should be in journal: {records:?}"
    );

    // reachable + resolve should match the checkpoint state (only a.txt).
    let reachable = journal::timeline::reachable(records);
    let ch = journal::resolve::resolve(reachable).expect("resolve");
    let debug = format!("{ch:?}");
    assert!(
        debug.contains("a.txt"),
        "a.txt should be in live changes: {debug}"
    );
    assert!(
        !debug.contains("b.txt"),
        "b.txt should NOT be in live changes: {debug}"
    );
}

// ── Inode store after restore ────────────────────────────────────────────

/// Pre-checkpoint inodes are preserved after restore.
#[test]
fn restore_keeps_pre_checkpoint_inodes() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    let pre_ino = ino_for(&changes(&s), "/a.txt");
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");
    s.cli(&["restore", "chk1"]).expect("restore");

    assert!(
        inode_path(&s, pre_ino).exists(),
        "pre-checkpoint inode {pre_ino} should still exist on disk"
    );
    assert_eq!(
        fs::read_to_string(inode_path(&s, pre_ino)).unwrap(),
        "a\n",
        "pre-checkpoint inode content should be intact"
    );
}

/// Post-checkpoint inodes are orphaned but still on disk after restore.
#[test]
fn restore_orphans_post_checkpoint_inodes() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");
    let post_ino = ino_for(&changes(&s), "/b.txt");

    s.cli(&["restore", "chk1"]).expect("restore");

    // Inode file still on disk (orphaned)
    assert!(
        inode_path(&s, post_ino).exists(),
        "orphaned inode should still be on disk"
    );

    // But not referenced by any resolved change
    let ch = changes(&s);
    assert!(
        !ch.iter().any(|c| c.ino() == Some(post_ino)),
        "orphaned inode should not appear in resolved changes"
    );
}

/// Abort after restore cleans up all inodes including orphans.
#[test]
fn abort_after_restore_cleans_orphans() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");
    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");

    s.cli(&["restore", "chk1"]).expect("restore");
    s.cli(&["abort", "--force"]).expect("abort");

    assert!(
        inos(&s).is_empty(),
        "abort should clear all inodes including orphans"
    );
}

/// After a restore, newly created files must receive fresh (monotonically
/// increasing) inode numbers — orphaned post-checkpoint inodes must never
/// be recycled.
#[test]
fn restore_new_files_get_fresh_inodes() {
    let s = AgfsSession::new().expect("session setup");

    // Create a file and checkpoint.
    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

    // Post-checkpoint: create another file.
    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");
    let inos_before = inos(&s);
    let max_ino_before = *inos_before.last().expect("should have inodes");

    // Restore — b.txt's inode becomes orphaned.
    s.cli(&["restore", "chk1"]).expect("restore");

    // Create new files after restore.
    fs::write(s.mnt_path("c.txt"), "c\n").expect("write c");
    fs::write(s.mnt_path("d.txt"), "d\n").expect("write d");

    // Identify inodes allocated after restore.
    let inos_after = inos(&s);
    let fresh: Vec<u64> = inos_after
        .iter()
        .copied()
        .filter(|ino| !inos_before.contains(ino))
        .collect();

    // Must have at least 2 new inodes (for c.txt and d.txt).
    assert!(
        fresh.len() >= 2,
        "expected at least 2 fresh inodes, got {fresh:?}"
    );

    // Every new inode must be strictly greater than any pre-restore inode.
    for ino in &fresh {
        assert!(
            *ino > max_ino_before,
            "inode {ino} should be > {max_ino_before} (inodes must not be recycled)"
        );
    }
}

// ── Re-COW behavior after restore ────────────────────────────────────────

/// Writing to a restored file without a new checkpoint reuses the inode.
#[test]
fn write_after_restore_without_checkpoint_reuses_inode() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("file.txt"), "v1\n").expect("write v1");
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

    fs::write(s.mnt_path("file.txt"), "v2\n").expect("write v2");
    s.cli(&["restore", "chk1"]).expect("restore");

    // checkpoint_gen is now set to chk1's gen, and the dirent's
    // checkpoint_gen matches. So writing should open the inode directly
    // (truncate in place), not allocate a new one.
    let inos_before = inos(&s);
    fs::write(s.mnt_path("file.txt"), "v1-modified\n").expect("write after restore");
    let inos_after = inos(&s);

    assert_eq!(
        inos_before.len(),
        inos_after.len(),
        "no new inode should be allocated: before={inos_before:?}, after={inos_after:?}"
    );
}

/// Writing after restore + new checkpoint triggers re-COW (new inode).
#[test]
fn write_after_restore_and_checkpoint_triggers_recow() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("file.txt"), "v1\n").expect("write v1");
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");
    fs::write(s.mnt_path("file.txt"), "v2\n").expect("write v2");

    s.cli(&["restore", "chk1"]).expect("restore");
    s.cli(&["checkpoint", "post-restore"])
        .expect("new checkpoint");

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

/// Re-COW after restore preserves the pre-checkpoint inode content.
#[test]
fn recow_after_restore_preserves_old_inode() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("file.txt"), "v1\n").expect("write v1");
    let v1_ino = ino_for(&changes(&s), "/file.txt");
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

    fs::write(s.mnt_path("file.txt"), "v2\n").expect("write v2 (re-COW)");

    s.cli(&["restore", "chk1"]).expect("restore");
    s.cli(&["checkpoint", "post-restore"])
        .expect("new checkpoint");

    fs::write(s.mnt_path("file.txt"), "v3\n").expect("write v3 (re-COW)");

    // The v1 inode should still have the original content
    assert_eq!(
        fs::read_to_string(inode_path(&s, v1_ino)).unwrap(),
        "v1\n",
        "original inode should be untouched after re-COW"
    );
}

// ── Resolved state correctness after restore ─────────────────────────────

/// Resolved changes after restore exactly match the checkpoint state.
#[test]
fn resolved_changes_match_checkpoint_state() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

    fs::write(s.mnt_path("c.txt"), "c\n").expect("write c");
    fs::remove_file(s.mnt_path("a.txt")).expect("delete a");

    s.cli(&["restore", "chk1"]).expect("restore");

    let ch = changes(&s);
    assert_eq!(ch.len(), 2, "exactly 2 changes: {ch:?}");

    let debug = format!("{ch:?}");
    assert!(
        debug.contains("a.txt"),
        "a.txt should be in changes: {debug}"
    );
    assert!(
        debug.contains("b.txt"),
        "b.txt should be in changes: {debug}"
    );
    assert!(
        !debug.contains("c.txt"),
        "c.txt should NOT be in changes: {debug}"
    );
}

// ── Renamed d_type correctness after restore ─────────────────────────────

/// Renamed directory resolves to a Renamed change after restore.
#[test]
fn restore_renamed_directory_in_resolved_changes() {
    let s = AgfsSession::new().expect("session setup");

    // Create directory in base first via commit
    fs::create_dir(s.mnt_path("old_dir")).expect("mkdir");
    fs::write(s.mnt_path("old_dir/inner.txt"), "inner\n").expect("write");
    s.cli(&["commit"]).expect("commit to base");

    // Now rename the base directory through the mount
    fs::rename(s.mnt_path("old_dir"), s.mnt_path("new_dir")).expect("rename dir");
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

    fs::write(s.mnt_path("extra.txt"), "extra\n").expect("write post");

    s.cli(&["restore", "chk1"]).expect("restore");

    let ch = changes(&s);
    let debug = format!("{ch:?}");

    // The rename should survive restore
    assert!(
        debug.contains("new_dir"),
        "new_dir should be in changes: {debug}"
    );
    assert!(
        !debug.contains("extra.txt"),
        "post-checkpoint file should NOT be in changes: {debug}"
    );

    // Verify the directory is accessible and d_type is dir via symlink_metadata
    let meta = fs::symlink_metadata(s.mnt_path("new_dir")).expect("lstat new_dir");
    assert!(
        meta.file_type().is_dir(),
        "new_dir should be a directory after restore"
    );
    assert_eq!(
        fs::read_to_string(s.mnt_path("new_dir/inner.txt")).unwrap(),
        "inner\n"
    );
}

/// Renamed symlink resolves correctly after restore and inode store is consistent.
#[test]
fn restore_renamed_symlink_in_resolved_changes() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("target.txt"), "target\n").expect("write target");
    std::os::unix::fs::symlink("target.txt", s.mnt_path("old_link")).expect("symlink");
    fs::rename(s.mnt_path("old_link"), s.mnt_path("new_link")).expect("rename symlink");
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

    fs::write(s.mnt_path("post.txt"), "post\n").expect("write post");

    s.cli(&["restore", "chk1"]).expect("restore");

    let ch = changes(&s);
    let debug = format!("{ch:?}");

    assert!(
        debug.contains("new_link"),
        "new_link should be in changes: {debug}"
    );
    assert!(
        !debug.contains("post.txt"),
        "post-checkpoint file should NOT be in changes: {debug}"
    );

    // Verify d_type is symlink via lstat through the mount
    let meta = fs::symlink_metadata(s.mnt_path("new_link")).expect("lstat new_link");
    assert!(
        meta.file_type().is_symlink(),
        "new_link should be a symlink after restore"
    );

    // Journal should have the checkpoint marker and records up to it
    let records = journal(&s);
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Checkpoint(c) if c.name == "chk1")),
        "chk1 should be in journal: {records:?}"
    );
}

// ── Journal inode and byte-level invariants ──────────────────────────────

/// The journal file inode must be preserved across restore (set_len, not replace).
#[test]
fn restore_preserves_journal_inode() {
    let s = AgfsSession::new().expect("session setup");

    let journal_path = s.root.join(".agfs/journal");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");
    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");

    let ino_before = fs::metadata(&journal_path).expect("stat before").ino();

    s.cli(&["restore", "chk1"]).expect("restore");

    let ino_after = fs::metadata(&journal_path).expect("stat after").ino();
    assert_eq!(
        ino_before, ino_after,
        "journal inode must be preserved across restore"
    );
}

/// After restore, the journal grows (S record appended) and original bytes are preserved.
#[test]
fn restore_journal_is_byte_prefix() {
    let s = AgfsSession::new().expect("session setup");

    let journal_path = s.root.join(".agfs/journal");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");
    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");

    let bytes_before = fs::read(&journal_path).expect("read before");

    s.cli(&["restore", "chk1"]).expect("restore");

    let bytes_after = fs::read(&journal_path).expect("read after");
    assert!(
        bytes_after.len() > bytes_before.len(),
        "journal should grow after restore (S record appended): before={} after={}",
        bytes_before.len(),
        bytes_after.len()
    );
    assert_eq!(
        &bytes_after[..bytes_before.len()],
        &bytes_before[..],
        "original journal bytes must be preserved as a prefix"
    );

    // Verify the S record is present.
    let records = journal(&s);
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Restore { .. })),
        "Restore record should be in journal: {records:?}"
    );
}

/// The S record written by restore should have a gen_id higher than the
/// target checkpoint's gen_id (monotonically increasing).
#[test]
fn restore_s_record_has_correct_gen() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("a.txt"), "a\n").expect("write a");
    s.cli(&["checkpoint", "chk1"]).expect("checkpoint");

    fs::write(s.mnt_path("b.txt"), "b\n").expect("write b");
    s.cli(&["checkpoint", "chk2"]).expect("checkpoint 2");

    s.cli(&["restore", "chk1"]).expect("restore");

    let records = journal(&s);

    // Find the checkpoint gen_ids and the restore record.
    let chk1_gen = records.iter().find_map(|r| match r {
        Record::Checkpoint(c) if c.name == "chk1" => Some(c.gen_id),
        _ => None,
    }).expect("chk1 should exist");

    let chk2_gen = records.iter().find_map(|r| match r {
        Record::Checkpoint(c) if c.name == "chk2" => Some(c.gen_id),
        _ => None,
    }).expect("chk2 should exist");

    let (s_gen, s_target) = records.iter().find_map(|r| match r {
        Record::Restore { gen_id, target_gen } => Some((*gen_id, *target_gen)),
        _ => None,
    }).expect("restore record should exist");

    assert_eq!(s_target, chk1_gen, "S record should target chk1");
    assert!(
        s_gen > chk2_gen,
        "S record gen ({s_gen}) should be greater than chk2 gen ({chk2_gen})"
    );
}
