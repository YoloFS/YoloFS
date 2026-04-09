use super::helpers::{actions, ino_for, inode_path, inos, journal, tree};
use crate::helpers::YoloSession;
use yolofs::journal::Action;
use std::fs;

#[test]
fn lookup_miss_creates_no_staging_state() {
    let s = YoloSession::new().expect("session setup");
    let missing = s.mnt_path("missing.txt");
    let inos_before = inos(&s);
    let action_count_before = actions(&journal(&s)).len();

    let err = fs::metadata(&missing).unwrap_err();
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::NotFound,
        "lookup miss should return ENOENT"
    );

    assert_eq!(
        inos(&s),
        inos_before,
        "lookup miss should not allocate staged inodes"
    );
    assert_eq!(
        actions(&journal(&s)).len(),
        action_count_before,
        "lookup miss should not append journal records"
    );
}

#[test]
fn lookup_miss_then_create_produces_single_add() {
    let s = YoloSession::new().expect("session setup");
    let created = s.mnt_path("after-miss.txt");
    let inos_before = inos(&s);

    let err = fs::metadata(&created).unwrap_err();
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::NotFound,
        "lookup miss should return ENOENT"
    );

    fs::write(&created, "created after miss\n").expect("create after miss");

    let t = tree(&s);
    let ino = ino_for(&t, "/after-miss.txt");
    let j = journal(&s);
    let acts = actions(&j);
    let adds: Vec<_> = acts
        .iter()
        .filter(|a| matches!(a, Action::Stage { path, .. } if path.ends_with("/after-miss.txt")))
        .collect();

    assert_eq!(
        inos(&s).len(),
        inos_before.len() + 1,
        "create after miss should allocate exactly one staged inode"
    );
    assert_eq!(
        fs::read_to_string(inode_path(&s, ino)).unwrap(),
        "created after miss\n",
        "staged inode should back the recreated file"
    );
    assert_eq!(
        adds.len(),
        1,
        "create after miss should append exactly one Stage record: {acts:?}"
    );
}

#[test]
fn tombstone_lookup_then_recreate_gets_fresh_inode() {
    let s = YoloSession::new().expect("session setup");
    let path = s.mnt_path("hello.txt");
    let inos_before = inos(&s);

    fs::remove_file(&path).expect("delete base file");

    let err = fs::metadata(&path).unwrap_err();
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::NotFound,
        "tombstone lookup should return ENOENT"
    );

    fs::write(&path, "reborn after tombstone lookup\n").expect("recreate over tombstone");

    let t = tree(&s);
    let ino = ino_for(&t, "/hello.txt");

    assert_eq!(
        fs::read_to_string(inode_path(&s, ino)).unwrap(),
        "reborn after tombstone lookup\n",
        "recreated file should be backed by the new staged inode"
    );
    assert_eq!(
        inos(&s).len(),
        inos_before.len() + 1,
        "delete should not allocate an inode, but recreate should allocate one"
    );
}
