// agfs — kernel log capture via /dev/kmsg.
//
// Reads kernel messages directly from the ring buffer without requiring
// systemd.  Used by both the integration test helpers and the bench binary
// to capture kernel messages produced during a run.

/// A snapshot of the kernel ring buffer position, identified by the sequence
/// number of the last record at the time of the snapshot.
pub struct KmsgCursor {
    seq: u64,
}

impl KmsgCursor {
    /// Snapshot the current tail of the kernel ring buffer.
    ///
    /// Opens `/dev/kmsg`, reads every record (non-blocking) to find the last
    /// sequence number, and returns a cursor pointing at it.  Returns `None`
    /// if `/dev/kmsg` cannot be opened or the ring buffer is empty.
    pub fn now() -> Option<Self> {
        let fd = kmsg_open()?;
        let mut last_seq: Option<u64> = None;
        let mut buf = [0u8; 8192];

        loop {
            match kmsg_read(fd, &mut buf) {
                ReadResult::Record(n) => {
                    if let Some((seq, _)) = parse_record(&buf[..n]) {
                        last_seq = Some(seq);
                    }
                }
                ReadResult::Skip => continue,
                ReadResult::Done => break,
            }
        }

        // SAFETY: fd is a valid file descriptor we opened above.
        unsafe { libc::close(fd) };
        last_seq.map(|seq| KmsgCursor { seq })
    }

    /// Return all kernel messages that arrived after this cursor.
    pub fn read_new(&self) -> Vec<String> {
        let Some(fd) = kmsg_open() else {
            return Vec::new();
        };

        let mut messages = Vec::new();
        let mut buf = [0u8; 8192];

        loop {
            match kmsg_read(fd, &mut buf) {
                ReadResult::Record(n) => {
                    if let Some((seq, msg)) = parse_record(&buf[..n])
                        && seq > self.seq
                    {
                        messages.push(msg);
                    }
                }
                ReadResult::Skip => continue,
                ReadResult::Done => break,
            }
        }

        unsafe { libc::close(fd) };
        messages
    }
}

/// Open `/dev/kmsg` read-only and non-blocking.  Returns the raw fd.
fn kmsg_open() -> Option<i32> {
    let fd = unsafe { libc::open(c"/dev/kmsg".as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
    if fd < 0 {
        eprintln!(
            "kmsg: could not open /dev/kmsg: {}",
            std::io::Error::last_os_error()
        );
        return None;
    }
    Some(fd)
}

enum ReadResult {
    /// Successfully read `n` bytes into the buffer.
    Record(usize),
    /// The read position was overrun by the ring buffer wrapping; retry.
    Skip,
    /// No more records available (EAGAIN).
    Done,
}

/// Read a single record from an open `/dev/kmsg` fd.
fn kmsg_read(fd: i32, buf: &mut [u8]) -> ReadResult {
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n > 0 {
        return ReadResult::Record(n as usize);
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::EAGAIN) => ReadResult::Done,
        Some(libc::EPIPE) => ReadResult::Skip,
        _ => ReadResult::Done,
    }
}

/// Parse a `/dev/kmsg` record.
///
/// Format: `level,sequence,timestamp[,flags];message\n`
/// Returns `(sequence, message_text)`.
fn parse_record(buf: &[u8]) -> Option<(u64, String)> {
    let text = std::str::from_utf8(buf).ok()?;
    let (header, rest) = text.split_once(';')?;
    let message = rest.split('\n').next().unwrap_or("");
    let mut parts = header.split(',');
    let _level = parts.next()?;
    let seq: u64 = parts.next()?.parse().ok()?;
    Some((seq, message.to_string()))
}
