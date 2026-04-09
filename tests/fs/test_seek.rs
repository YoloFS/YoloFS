use crate::helpers::YoloSession;
use std::io::{Read, Seek, SeekFrom, Write};

/// SEEK_CUR after write should use the updated file position.
#[test]
fn seek_cur_after_write() {
    let s = YoloSession::new().expect("session setup");
    let path = s.mnt_path("seek_test.bin");

    let mut f = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("create file");

    // Write 100 bytes.
    f.write_all(&[0u8; 100]).expect("write 100 bytes");
    assert_eq!(f.stream_position().unwrap(), 100);

    // Seek back 50 from current.
    let pos = f
        .seek(SeekFrom::Current(-50))
        .expect("seek -50 from current");
    assert_eq!(pos, 50, "SEEK_CUR(-50) from pos 100 should give 50");
}

/// SEEK_CUR after write-then-seek-to-start-then-write should track correctly.
/// This is the pattern that causes the assembler (as) to fail: write, seek to 0,
/// write header, then seek backwards from current position.
#[test]
fn seek_cur_after_rewind_and_write() {
    let s = YoloSession::new().expect("session setup");
    let path = s.mnt_path("seek_rewind.bin");

    let mut f = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("create file");

    // Seek forward, write at offset 120.
    f.seek(SeekFrom::Start(120)).unwrap();
    f.write_all(b"\0main\0").expect("write at 120");
    assert_eq!(f.stream_position().unwrap(), 126);

    // Rewind to start, write 64-byte header.
    f.seek(SeekFrom::Start(0)).unwrap();
    f.write_all(&[0x42u8; 64]).expect("write header");
    assert_eq!(f.stream_position().unwrap(), 64);

    // Seek back 62 from current (64 - 62 = 2).
    let pos = f
        .seek(SeekFrom::Current(-62))
        .expect("seek -62 from current");
    assert_eq!(pos, 2, "SEEK_CUR(-62) from pos 64 should give 2");
}

/// SEEK_SET and SEEK_END should work on staged files.
#[test]
fn seek_set_and_end() {
    let s = YoloSession::new().expect("session setup");
    let path = s.mnt_path("seek_ends.bin");

    let mut f = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("create file");

    f.write_all(&[0u8; 200]).expect("write 200 bytes");

    // SEEK_SET to 50.
    let pos = f.seek(SeekFrom::Start(50)).expect("seek to 50");
    assert_eq!(pos, 50);

    // SEEK_END to -10.
    let pos = f.seek(SeekFrom::End(-10)).expect("seek to end-10");
    assert_eq!(pos, 190);
}

/// SEEK_CUR after read should also track the position correctly.
#[test]
fn seek_cur_after_read() {
    let s = YoloSession::new().expect("session setup");
    let path = s.mnt_path("seek_read.bin");

    // Write 100 bytes.
    std::fs::write(&path, &[0xABu8; 100]).expect("write file");

    let mut f = std::fs::File::open(&path).expect("open for read");

    // Read 30 bytes.
    let mut buf = [0u8; 30];
    f.read_exact(&mut buf).expect("read 30");

    // SEEK_CUR(-10) should give 20.
    let pos = f
        .seek(SeekFrom::Current(-10))
        .expect("seek -10 from current");
    assert_eq!(pos, 20);
}

/// SEEK_CUR on a COW file (modified base file) should work.
#[test]
fn seek_cur_on_cow_file() {
    let s = YoloSession::new().expect("session setup");
    let path = s.mnt_path("hello.txt"); // base file, will COW on write

    let mut f = std::fs::File::options()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open for rw (triggers COW)");

    f.write_all(b"overwritten content\n").expect("write");
    let pos = f.stream_position().unwrap();
    assert!(pos > 0, "position should be nonzero after write");

    let new_pos = f.seek(SeekFrom::Current(-5)).expect("seek -5 from current");
    assert_eq!(new_pos, pos - 5);
}
