use super::helpers::{actions, journal, records};
use crate::helpers::AgfsSession;
use agfs::journal::{Action, Marker, Record};
use std::fs;

// ── Tests for the `overwrites` dirent flag ──────────────────────────────────
//
// The kernel tracks whether a path had existing content via an `overwrites`
// flag on each dirent. This flag determines the journal record tag:
// ADD (add, new path) vs MOD (modify, overwrites content). These tests verify
// that the kernel emits the correct tag for various edge cases.

/// Creating a brand-new file emits an ADD (add) record.
#[test]
fn create_new_file_emits_add() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("brandnew.txt"), "new\n").expect("create");

    let j = journal(&s);
    let acts = actions(&j);
    assert!(
        acts.iter()
            .any(|a| matches!(a, Action::Add { path, .. } if path.ends_with("/brandnew.txt"))),
        "new file should produce ADD record: {acts:?}"
    );
}

/// Modifying an existing base file emits a MOD (modify) record.
#[test]
fn modify_base_file_emits_modify() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "changed\n").expect("write");

    let j = journal(&s);
    let acts = actions(&j);
    assert!(
        acts.iter()
            .any(|a| matches!(a, Action::Modify { path, .. } if path.ends_with("/hello.txt"))),
        "modifying base file should produce MOD record: {acts:?}"
    );
}

/// Delete a base file then re-create it: the re-create should emit MOD
/// (the path existed in base), not ADD.
#[test]
fn delete_recreate_base_file_emits_modify() {
    let s = AgfsSession::new().expect("session setup");

    fs::remove_file(s.mnt_path("hello.txt")).expect("delete");
    fs::write(s.mnt_path("hello.txt"), "reborn\n").expect("recreate");

    let j = journal(&s);
    let acts = actions(&j);
    // Find the last record for hello.txt — should be M (from re-create).
    let last = acts
        .iter()
        .rev()
        .find(|a| match a {
            Action::Add { path, .. } | Action::Modify { path, .. } => {
                path.ends_with("/hello.txt")
            }
            _ => false,
        })
        .expect("should have a staged record for hello.txt");
    assert!(
        matches!(last, Action::Modify { .. }),
        "re-create of base file should produce MOD, got: {last:?}"
    );
}

/// Delete a newly-created file then re-create it: the re-create should
/// emit ADD (the path never existed in base).
#[test]
fn delete_recreate_staged_file_emits_add() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("ephemeral.txt"), "v1\n").expect("create");
    fs::remove_file(s.mnt_path("ephemeral.txt")).expect("delete");
    fs::write(s.mnt_path("ephemeral.txt"), "v2\n").expect("recreate");

    let j = journal(&s);
    let acts = actions(&j);
    // Find the last record for ephemeral.txt — should be ADD.
    let last = acts
        .iter()
        .rev()
        .find(|a| match a {
            Action::Add { path, .. } | Action::Modify { path, .. } => {
                path.ends_with("/ephemeral.txt")
            }
            _ => false,
        })
        .expect("should have a staged record for ephemeral.txt");
    assert!(
        matches!(last, Action::Add { .. }),
        "re-create of staged-only file should produce ADD, got: {last:?}"
    );
}

/// Modify a base file (COW), delete it, re-create: should emit MOD
/// because the path existed in base.
#[test]
fn cow_delete_recreate_emits_modify() {
    let s = AgfsSession::new().expect("session setup");

    // COW (modifies existing base file)
    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("cow");
    // Delete the now-staged file
    fs::remove_file(s.mnt_path("hello.txt")).expect("delete");
    // Re-create at the same path
    fs::write(s.mnt_path("hello.txt"), "again\n").expect("recreate");

    let j = journal(&s);
    let acts = actions(&j);
    let last = acts
        .iter()
        .rev()
        .find(|a| match a {
            Action::Add { path, .. } | Action::Modify { path, .. } => {
                path.ends_with("/hello.txt")
            }
            _ => false,
        })
        .expect("should have a staged record for hello.txt");
    assert!(
        matches!(last, Action::Modify { .. }),
        "re-create after COW+delete of base file should produce MOD, got: {last:?}"
    );
}

/// Rename a base file away, then create at the old path: the create
/// should emit MOD (the path existed in base).
#[test]
fn rename_away_then_create_at_old_path_emits_modify() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
    fs::write(s.mnt_path("hello.txt"), "replacement\n").expect("create");

    let j = journal(&s);
    let acts = actions(&j);
    let last = acts
        .iter()
        .rev()
        .find(|a| match a {
            Action::Add { path, .. } | Action::Modify { path, .. } => {
                path.ends_with("/hello.txt")
            }
            _ => false,
        })
        .expect("should have a staged record for hello.txt");
    assert!(
        matches!(last, Action::Modify { .. }),
        "create at renamed-away base path should produce MOD, got: {last:?}"
    );
}

/// Rename a staged-only file away, then create at the old path: the
/// create should emit ADD (the path never existed in base).
#[test]
fn rename_away_staged_then_create_emits_add() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("temp.txt"), "staged\n").expect("create");
    fs::rename(s.mnt_path("temp.txt"), s.mnt_path("moved.txt")).expect("rename");
    fs::write(s.mnt_path("temp.txt"), "new\n").expect("recreate");

    let j = journal(&s);
    let acts = actions(&j);
    let last = acts
        .iter()
        .rev()
        .find(|a| match a {
            Action::Add { path, .. } | Action::Modify { path, .. } => {
                path.ends_with("/temp.txt")
            }
            _ => false,
        })
        .expect("should have a staged record for temp.txt");
    assert!(
        matches!(last, Action::Add { .. }),
        "create at renamed-away staged path should produce ADD, got: {last:?}"
    );
}

/// Create a new file, checkpoint, then write to it again (re-COW).
/// The re-COW should emit MOD (Modified) because the path already had
/// staged content — the `overwrites` flag is true regardless of whether
/// the file existed in base.
#[test]
fn recow_of_staged_file_after_checkpoint_emits_modify() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("created.txt"), "v1\n").expect("create");
    s.cli(&["checkpoint", "s1"]).expect("checkpoint");
    fs::write(s.mnt_path("created.txt"), "v2\n").expect("write v2 (re-COW)");

    let recs = records(&journal(&s));

    // Find all staged records for created.txt after the checkpoint.
    let chk_pos = recs
        .iter()
        .position(|r| matches!(r, Record::Marker(Marker::Checkpoint { name, .. }) if name == "s1"))
        .expect("should have checkpoint s1");
    let post_chk: Vec<_> = recs[chk_pos + 1..]
        .iter()
        .filter(|r| match r {
            Record::Action(Action::Add { path, .. }) | Record::Action(Action::Modify { path, .. }) => {
                path.ends_with("/created.txt")
            }
            _ => false,
        })
        .collect();

    assert!(
        !post_chk.is_empty(),
        "re-COW should produce a record after checkpoint: {recs:?}"
    );
    // Re-COW of a staged file emits MOD (overwrites=true) because the
    // path already had content, even though it was never in base.
    assert!(
        matches!(post_chk[0], Record::Action(Action::Modify { .. })),
        "re-COW of staged file should emit MOD (overwrites existing content), got: {:?}",
        post_chk[0]
    );
}
