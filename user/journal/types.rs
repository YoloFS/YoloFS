// yolo CLI — journal/types.rs
//
// Pure data types for journal records. No I/O dependencies.

/// The target of a dentry — where content lives.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// Content staged in flat file store at this inode ID.
    StagedFile(u32),
    /// Redirect: content lives at a path in the lower (base) filesystem.
    /// The path is static — it references the immutable base filesystem,
    /// not an overlay path created by a rename.
    BasePath(String),
    /// Content absent (deleted).
    Tombstone,
    /// No staged change (scaffold dir for deeper nodes).
    Passthrough,
}

impl Target {
    /// Return the staged inode ID if this target carries one.
    pub fn ino(&self) -> Option<u32> {
        match self {
            Target::StagedFile(ino) => Some(*ino),
            _ => None,
        }
    }

    /// True if this target involves the given path (as source or destination).
    pub fn matches_path(&self, dentry_path: &str, query: &str) -> bool {
        match self {
            Target::BasePath(src) => dentry_path == query || src == query,
            _ => dentry_path == query,
        }
    }
}

/// A data mutation applied to the dir tree (S/D/R).
#[derive(Debug, Clone)]
pub enum Action {
    /// `preimage` is the absolute path of the content this stage overwrote — the
    /// file COW copied up (a base file, a redirect-resolved backing, or a prior
    /// snapshot's staged inode), as it existed in the **previous snapshot**.
    /// `None` for a fresh create (nothing existed). It is *not* relative to the
    /// immutable base: a file created this session then modified after a snapshot
    /// has a `Some` pre-image (it existed in that prior snapshot). Its presence
    /// classifies added (`None`) vs modified (`Some`), and `diff` reads it for
    /// the old content — so status/diff need neither a separate `existed` bit nor
    /// a rebuilt previous tree (O(segment), not O(journal)). Resolved from the
    /// first touch in changeset.rs. A pre-image the kernel can't resolve (a path
    /// over YOLO_PATH_MAX, or an unreachable lower) is recorded empty → `None`,
    /// so that change reads as added rather than modified.
    Stage {
        path: String,
        ino: u32,
        preimage: Option<String>,
    },
    /// `preimage` is the absolute path of the removed content (as in the previous
    /// snapshot), or `None` when the path was created and deleted within the
    /// range (a no-op delete). Lets `diff` show the deleted content in O(segment).
    Delete {
        path: String,
        preimage: Option<String>,
    },
    Rename {
        src: String,
        dst: String,
    },
}

/// A control marker (P/T).
#[derive(Debug, Clone)]
pub enum Marker {
    Snapshot { gen_id: u64, name: String },
    Travel { gen_id: u64, target_gen: u64 },
}

/// The operation an access attempted, as recorded in a note's `op` field.
/// Journal encoding: `r` / `w`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Read,
    Write,
}

impl Op {
    /// Parse the journal's single-letter op code.
    pub fn from_byte(b: u8) -> Option<Op> {
        match b {
            b'r' => Some(Op::Read),
            b'w' => Some(Op::Write),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Op::Read => "read",
            Op::Write => "write",
        }
    }
}

/// An observational note — does not affect overlay state, only audit.
///
/// Emitted by the kernel and ignored by commit/abort/diff/replay; only
/// `yolo journal` surfaces them. The `decision` is a [`Perm`](crate::perm::Perm)
/// (the unified permission type); journal-encoded as a single letter.
///
/// - `Ask` — an `ask` path was resolved to `decision` (by the daemon or the
///   timeout default). Wire: `A\0<path>\0<op>\0<decision>\n`.
/// - `Block` — a rule returned `-EACCES`. Wire: `B\0<path>\0<op>\n`.
#[derive(Debug, Clone)]
pub enum Note {
    Ask {
        path: String,
        op: Op,
        decision: crate::perm::Perm,
    },
    Block {
        path: String,
        op: Op,
    },
}

/// A parsed journal record (interleaved actions, markers, and notes).
#[derive(Debug, Clone)]
pub enum Record {
    Action(Action),
    Marker(Marker),
    Note(Note),
}

/// A group of records (S/D/R + B) between consecutive P/T boundaries.
///
/// `Record::Marker` is never pushed into a segment by `Journal::new` —
/// markers split segments. Only `Record::Action` and `Record::Note`
/// appear here.
#[derive(Debug)]
pub struct Segment {
    /// The gen_id of the snapshot this segment builds on.
    /// 0 for the 0-th segment (records before the first snapshot).
    pub from: u64,
    /// The S/D/R + B records in this segment (no P/T records).
    pub records: Vec<Record>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_byte_roundtrips() {
        for op in [Op::Read, Op::Write] {
            let letter = match op {
                Op::Read => b'r',
                Op::Write => b'w',
            };
            assert_eq!(Op::from_byte(letter), Some(op));
        }
        assert_eq!(Op::from_byte(b'x'), None);
        assert_eq!(Op::Read.label(), "read");
        assert_eq!(Op::Write.label(), "write");
    }
}
