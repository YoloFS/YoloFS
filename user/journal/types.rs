// yolo CLI — journal/types.rs
//
// Pure data types for journal records. No I/O dependencies.

/// Where content lives at a path — the state at a dentry/tree node, and the
/// value of a journal record's pre-op `pre` fields (the parser resolves the
/// tagged wire form `a`/`s:<ino>`/`b:<path>` into this). One value type for both
/// the operation-local axis (`pre`) and the range-scoped tree axis
/// (`old`/`new`); the role is carried by the field name, not the type.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// Content staged in flat file store at this inode ID.
    StagedFile(u32),
    /// Redirect: content lives at a path in the base (lower) filesystem. Only
    /// ever a real base-filesystem path — staged content resolves to
    /// `StagedFile` at parse time, never a `.yolofs/inodes/` path here.
    BasePath(String),
    /// Content absent — a deletion marker / nothing here.
    Absence,
}

impl Target {
    /// Return the staged inode ID if this target carries one.
    pub fn ino(&self) -> Option<u32> {
        match self {
            Target::StagedFile(ino) => Some(*ino),
            _ => None,
        }
    }
}

/// A data mutation applied to the dir tree (S/D/R).
///
/// Each `*pre` field is the operation-local pre-op backing of that overlay name
/// — the `Target` the kernel resolved immediately before the op (see
/// [`Target`]). It seeds the range-start `old` side during the fold (first touch
/// wins) and `diff` reads it for the old content, so status/diff need neither a
/// rebuilt previous tree nor a base stat (O(segment), not O(journal)). For an
/// already-staged file it is the staged inode (`StagedFile`), not the base it
/// was COW'd from; a pre the kernel could not resolve is `Absence`.
#[derive(Debug, Clone)]
pub enum Action {
    /// Stage (create or COW). The post-target is `StagedFile(ino)`; `pre` is the
    /// content this stage overwrote (`Absence` for a fresh create).
    Stage { path: String, ino: u32, pre: Target },
    /// Delete. The post-target is `Absence`; `pre` is the removed content.
    Delete { path: String, pre: Target },
    /// Rename. `src_pre`/`dst_pre` are the source's and destination's pre-op
    /// backings (the destination's is `Absence` for a fresh name or a tombstone).
    Rename {
        src: String,
        dst: String,
        src_pre: Target,
        dst_pre: Target,
    },
}

/// A control marker (P/T).
#[derive(Debug, Clone)]
pub enum Marker {
    Snapshot { name: String },
    Travel { target_gen: u64 },
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
/// Emitted by the kernel and ignored by commit/abort/diff/replay; review
/// summaries and `yolo journal` surface them. The `decision` is a
/// [`Decision`](crate::perm::Decision), journal-encoded as a single letter.
///
/// - `Ask` — an `ask` path was resolved to `decision` (by the daemon or the
///   timeout default). Wire: `A\0<path>\0<op>\0<decision>\n`.
/// - `Block` — a rule returned `-EACCES`. Wire: `B\0<path>\0<op>\n`.
#[derive(Debug, Clone)]
pub enum Note {
    Ask {
        path: String,
        op: Op,
        decision: crate::perm::Decision,
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
