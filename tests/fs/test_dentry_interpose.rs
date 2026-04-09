use crate::helpers::YoloSession;
use std::fs;

#[test]
fn create_after_lookup_miss() {
    let s = YoloSession::new().expect("session setup");
    let path = s.mnt_path("brandnew-after-miss.txt");

    let err = fs::metadata(&path).unwrap_err();
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::NotFound,
        "lookup miss should return ENOENT"
    );

    fs::write(&path, "new content after miss\n").expect("create after miss");

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "new content after miss\n",
        "file should be readable immediately after create"
    );
}

#[test]
fn recreate_after_tombstone_lookup() {
    let s = YoloSession::new().expect("session setup");
    let path = s.mnt_path("hello.txt");

    fs::remove_file(&path).expect("delete base file");

    let err = fs::metadata(&path).unwrap_err();
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::NotFound,
        "tombstone lookup should return ENOENT"
    );

    fs::write(&path, "reborn after tombstone lookup\n").expect("recreate over tombstone");

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "reborn after tombstone lookup\n",
        "recreated file should be visible through the mount"
    );
}
