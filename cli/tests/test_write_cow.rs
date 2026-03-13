use crate::helpers::AgfsSession;
use std::fs;

#[test]
fn write_triggers_cow() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "modified\n").expect("write through mount");

    // Read through mount sees new content
    let content = fs::read_to_string(s.mnt_path("hello.txt")).expect("read");
    assert_eq!(content, "modified\n");

    // Base file unchanged
    let base = fs::read_to_string(s.base_path("hello.txt")).expect("read base");
    assert_eq!(base, "base content\n");

    // Staging has a blob with the COW copy
    let staging = s.staging_dir();
    let blob_content: Vec<String> = fs::read_dir(&staging)
        .unwrap()
        .filter_map(|e| {
            let e = e.ok()?;
            let name = e.file_name().to_string_lossy().to_string();
            name.parse::<u64>().ok()?;
            fs::read_to_string(e.path()).ok()
        })
        .collect();
    assert!(
        blob_content.iter().any(|c| c == "modified\n"),
        "staging should contain a blob with the modified content"
    );
}

#[test]
fn write_nested_file() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("subdir/deep.txt"), "changed\n").expect("write nested");

    let content = fs::read_to_string(s.mnt_path("subdir/deep.txt")).unwrap();
    assert_eq!(content, "changed\n");

    // Base unchanged
    let base = fs::read_to_string(s.base_path("subdir/deep.txt")).unwrap();
    assert_eq!(base, "nested\n");
}

#[test]
fn multiple_writes_same_file() {
    let s = AgfsSession::new().expect("session setup");

    fs::write(s.mnt_path("hello.txt"), "first\n").unwrap();
    fs::write(s.mnt_path("hello.txt"), "second\n").unwrap();

    let content = fs::read_to_string(s.mnt_path("hello.txt")).unwrap();
    assert_eq!(content, "second\n");

    // Base still original
    assert_eq!(
        fs::read_to_string(s.base_path("hello.txt")).unwrap(),
        "base content\n"
    );
}
