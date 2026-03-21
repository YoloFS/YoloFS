use super::helpers::journal;
use crate::helpers::AgfsSession;
use agfs::journal::{Action, Marker, Record};
use std::fs;

// ── Compound journal operations ──────────────────────────────────────────────
//
// These verify that compound filesystem operations produce the expected
// journal record sequence.  The CLI resolver collapses these into final
// changes; here we verify the raw kernel output.

/// Multiple operations produce records in order.
#[test]
fn operations_produce_ordered_records() {
    let s = AgfsSession::new().expect("session setup");

    // write → checkpoint → delete → rename
    fs::write(s.mnt_path("hello.txt"), "v1\n").expect("write");
    s.cli(&["checkpoint", "s1"]).expect("checkpoint");
    fs::remove_file(s.mnt_path("subdir/deep.txt")).expect("delete");
    fs::rename(s.mnt_path("multi.txt"), s.mnt_path("renamed.txt")).expect("rename");

    let records = journal(&s);

    // Verify each type is present
    assert!(
        records.iter().any(|r| matches!(r, Record::Action(Action::Modify { .. }))),
        "missing MOD: {records:?}"
    );
    assert!(
        records.iter().any(|r| matches!(r, Record::Marker(Marker::Checkpoint { .. }))),
        "missing CKP: {records:?}"
    );
    assert!(
        records.iter().any(|r| matches!(r, Record::Action(Action::Delete { .. }))),
        "missing DEL: {records:?}"
    );
    assert!(
        records.iter().any(|r| matches!(r, Record::Action(Action::Rename { .. }))),
        "missing RDR: {records:?}"
    );

    // Checkpoint "s1" should appear after the Add (write) and before the Delete.
    let chk_pos = records
        .iter()
        .position(|r| matches!(r, Record::Marker(Marker::Checkpoint { name, .. }) if name == "s1"))
        .unwrap();
    let add_pos = records
        .iter()
        .position(|r| matches!(r, Record::Action(Action::Modify { path, .. }) if path.ends_with("/hello.txt")))
        .unwrap();
    let del_pos = records
        .iter()
        .position(|r| matches!(r, Record::Action(Action::Delete { .. })))
        .unwrap();
    assert!(add_pos < chk_pos, "Add should precede Checkpoint s1");
    assert!(chk_pos < del_pos, "Checkpoint s1 should precede Delete");
}

/// Writing to a renamed file: rename produces RDR, then write produces MOD at new path.
#[test]
fn write_after_rename() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
    fs::write(s.mnt_path("moved.txt"), "updated\n").expect("write renamed file");

    let records = journal(&s);
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Action(Action::Rename { src, dst, .. })
            if dst.ends_with("/moved.txt") && src.ends_with("/hello.txt"))),
        "should have RDR record: {records:?}"
    );
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Action(Action::Modify { path, .. }) if path.ends_with("/moved.txt"))),
        "should have MOD record at new path: {records:?}"
    );

    // The rename should precede the write
    let r_pos = records
        .iter()
        .position(|r| matches!(r, Record::Action(Action::Rename { .. })))
        .unwrap();
    let a_pos = records
        .iter()
        .rposition(|r| matches!(r, Record::Action(Action::Modify { path, .. }) if path.ends_with("/moved.txt")))
        .unwrap();
    assert!(
        r_pos < a_pos,
        "Rename should precede the Modify at new path"
    );
}

/// Create a new file, then rename it.
/// All renames now emit a single R record.
#[test]
fn create_then_rename() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("temp.txt"), "ephemeral\n").expect("create");
    fs::rename(s.mnt_path("temp.txt"), s.mnt_path("final.txt")).expect("rename");

    let records = journal(&s);
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Action(Action::Add { path, .. }) if path.ends_with("/temp.txt"))),
        "should have ADD record for original path: {records:?}"
    );
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Action(Action::Rename { dst, src, .. })
            if dst.ends_with("/final.txt") && src.ends_with("/temp.txt"))),
        "should have Redirect record for temp.txt → final.txt: {records:?}"
    );
}

/// Create a file, then delete it — both ADD and DEL records should be present.
#[test]
fn create_then_delete() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("ephemeral.txt"), "gone soon\n").expect("create");
    fs::remove_file(s.mnt_path("ephemeral.txt")).expect("delete");

    let records = journal(&s);
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Action(Action::Add { path, .. }) if path.ends_with("/ephemeral.txt"))),
        "should have ADD record: {records:?}"
    );
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Action(Action::Delete { path, .. }) if path.ends_with("/ephemeral.txt"))),
        "should have DEL record: {records:?}"
    );
}

/// Modify a base file, then delete it — produces ADD then DEL.
#[test]
fn modify_then_delete() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write");
    fs::remove_file(s.mnt_path("hello.txt")).expect("delete");

    let records = journal(&s);
    let a_pos = records
        .iter()
        .position(|r| matches!(r, Record::Action(Action::Modify { path, .. }) if path.ends_with("/hello.txt")))
        .expect("missing MOD");
    let d_pos = records
        .iter()
        .position(|r| matches!(r, Record::Action(Action::Delete { path, .. }) if path.ends_with("/hello.txt")))
        .expect("missing DEL");
    assert!(a_pos < d_pos, "Add should precede Delete: {records:?}");
}

/// Rename a file, then delete the new name — produces RDR then DEL.
#[test]
fn rename_then_delete() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");
    fs::remove_file(s.mnt_path("moved.txt")).expect("delete renamed file");

    let records = journal(&s);
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Action(Action::Rename { src, dst, .. })
            if dst.ends_with("/moved.txt") && src.ends_with("/hello.txt"))),
        "should have RDR record: {records:?}"
    );
    assert!(
        records
            .iter()
            .any(|r| matches!(r, Record::Action(Action::Delete { path, .. }) if path.ends_with("/moved.txt"))),
        "should have DEL record at new path: {records:?}"
    );

    let r_pos = records
        .iter()
        .position(|r| matches!(r, Record::Action(Action::Rename { .. })))
        .expect("missing RDR");
    let d_pos = records
        .iter()
        .position(|r| matches!(r, Record::Action(Action::Delete { path, .. }) if path.ends_with("/moved.txt")))
        .expect("missing DEL");
    assert!(r_pos < d_pos, "Rename should precede Delete: {records:?}");
}

/// Kernel emits fused RDR record with both old and new paths (no separate DEL).
#[test]
fn rename_emits_fused_redirect_record() {
    let s = AgfsSession::new().expect("session setup");

    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("moved.txt")).expect("rename");

    let records = journal(&s);

    // Should have exactly one Redirect record with both old and new paths.
    let redirects: Vec<_> = records
        .iter()
        .filter(|r| matches!(r, Record::Action(Action::Rename { .. })))
        .collect();
    assert_eq!(
        redirects.len(),
        1,
        "expected exactly 1 Redirect record, got: {redirects:?}"
    );
    assert!(
        matches!(&redirects[0], Record::Action(Action::Rename { src, dst, .. })
            if src.ends_with("/hello.txt") && dst.ends_with("/moved.txt")),
        "Redirect should carry both old and new paths: {:?}",
        redirects[0]
    );

    // Should NOT have a separate Delete record for the old path.
    let has_delete_old = records.iter().any(|r| {
        matches!(r, Record::Action(Action::Delete { path, .. }) if path.ends_with("/hello.txt"))
    });
    assert!(
        !has_delete_old,
        "fused rename should NOT emit separate DEL record for old path: {records:?}"
    );
}

/// Overwrite rename (mv onto existing file) emits fused REP record.
#[test]
fn rename_overwrite_emits_fused_replace_record() {
    let s = AgfsSession::new().expect("session setup");

    // hello.txt and multi.txt both exist in base
    fs::rename(s.mnt_path("hello.txt"), s.mnt_path("multi.txt")).expect("overwrite rename");

    let records = journal(&s);

    let replaces: Vec<_> = records
        .iter()
        .filter(|r| matches!(r, Record::Action(Action::Replace { .. })))
        .collect();
    assert_eq!(
        replaces.len(),
        1,
        "expected exactly 1 Replace record, got: {replaces:?}"
    );
    assert!(
        matches!(&replaces[0], Record::Action(Action::Replace { src, dst, .. })
            if src.ends_with("/hello.txt") && dst.ends_with("/multi.txt")),
        "Replace should carry both old and new paths: {:?}",
        replaces[0]
    );
}
