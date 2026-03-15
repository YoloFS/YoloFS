// agfs — kernel log capture via the systemd journal.
//
// Used by both the integration test helpers and the bench binary to capture
// kernel messages produced during a run.

use systemd::journal::{self, JournalSeek};

/// Seek the system journal to its current tail and return a cursor pointing
/// at the last entry. Returns `None` if the journal is empty or unavailable.
pub fn snapshot() -> Option<String> {
    let mut j = journal::OpenOptions::default()
        .system(true)
        .local_only(true)
        .open()
        .map_err(|e| eprintln!("klog: could not open journal: {e}"))
        .ok()?;
    j.seek(JournalSeek::Tail).ok()?;
    let entry = j
        .previous_entry()
        .map_err(|e| eprintln!("klog: journal seek failed: {e}"))
        .ok()??;
    drop(entry);
    j.cursor()
        .map_err(|e| eprintln!("klog: could not read cursor: {e}"))
        .ok()
}

/// Return all kernel-transport messages that arrived after `cursor`.
pub fn since(cursor: &str) -> Vec<String> {
    let mut messages = Vec::new();
    let Ok(mut j) = journal::OpenOptions::default()
        .system(true)
        .local_only(true)
        .open()
        .map_err(|e| eprintln!("klog: could not open journal: {e}"))
    else {
        return messages;
    };
    if j.seek(JournalSeek::Cursor {
        cursor: cursor.to_string(),
    })
    .is_err()
    {
        return messages;
    }
    // Advance past the cursor entry before applying the filter so we don't
    // miss the first real kernel message.
    let _ = j.next_entry();
    let _ = j.match_add("_TRANSPORT", "kernel");
    while let Ok(Some(record)) = j.next_entry() {
        let msg = record
            .get("MESSAGE")
            .cloned()
            .unwrap_or_else(|| "<no message>".to_string());
        messages.push(msg);
    }
    messages
}
