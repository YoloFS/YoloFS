use crate::helpers::YoloSession;
use std::fs;
use std::thread;

#[test]
fn concurrent_writes_to_different_files() {
    let s = YoloSession::new().expect("session setup");

    let handles: Vec<_> = (0..10)
        .map(|i| {
            let path = s.mnt_path(&format!("concurrent_{i}.txt"));
            thread::spawn(move || {
                fs::write(&path, format!("content from thread {i}\n")).expect("write");
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread join");
    }

    // Verify all files exist with correct content
    for i in 0..10 {
        let content = fs::read_to_string(s.mnt_path(&format!("concurrent_{i}.txt"))).expect("read");
        assert_eq!(content, format!("content from thread {i}\n"));
    }
}

#[test]
fn concurrent_reads() {
    let s = YoloSession::new().expect("session setup");

    let handles: Vec<_> = (0..10)
        .map(|_| {
            let path = s.mnt_path("hello.txt");
            thread::spawn(move || fs::read_to_string(&path).expect("read"))
        })
        .collect();

    for h in handles {
        let content = h.join().expect("thread join");
        assert_eq!(content, "base content\n");
    }
}

#[test]
fn concurrent_create_and_readdir() {
    let s = YoloSession::new().expect("session setup");

    // Create several files first
    for i in 0..5 {
        fs::write(s.mnt_path(&format!("pre_{i}.txt")), "pre\n").expect("write");
    }

    let mnt = s.mnt_path("");
    let writer_mnt = s.mnt_path("");

    let reader = {
        let mnt = mnt.clone();
        thread::spawn(move || {
            // Read directory listing multiple times
            for _ in 0..5 {
                let _entries: Vec<String> = fs::read_dir(&mnt)
                    .expect("readdir")
                    .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
                    .collect();
            }
        })
    };

    let writer = thread::spawn(move || {
        for i in 5..10 {
            fs::write(writer_mnt.join(format!("dyn_{i}.txt")), "dyn\n").expect("write");
        }
    });

    reader.join().expect("reader join");
    writer.join().expect("writer join");

    // After both finish, all dynamic files should exist
    for i in 5..10 {
        assert!(
            s.mnt_path(&format!("dyn_{i}.txt")).exists(),
            "dyn_{i}.txt should exist"
        );
    }
}
