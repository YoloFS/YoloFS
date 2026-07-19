// yolo CLI — journal/types.rs
//
// Pure data types for journal records. No I/O dependencies.

/// Where content lives at a path — the state at a dentry/tree node, and the
/// value of a journal record's pre-op `pre` fields (the parser resolves the
/// tagged wire form `a`/`s:<ino>`/`b:<path>` into this). One value type for both
/// the operation-local axis (`pre`) and the range-scoped tree axis
/// (`old`/`new`); the role is carried by the field name, not the type.
#[derive(Debug, Clone, PartialEq)]
pub enum Backing {
    /// Content staged in flat file store at this inode ID.
    StagedFile(u32),
    /// Redirect: content lives at a path in the base (lower) filesystem. Only
    /// ever a real base-filesystem path — staged content resolves to
    /// `StagedFile` at parse time, never a `.yolofs/inodes/` path here.
    BasePath(String),
    /// Content absent — a deletion marker / nothing here.
    None,
}

impl Backing {
    /// Return the staged inode ID if this target carries one.
    pub fn ino(&self) -> Option<u32> {
        match self {
            Backing::StagedFile(ino) => Some(*ino),
            _ => None,
        }
    }
}

/// A data mutation applied to the dir tree (S/D/R).
///
/// Each `*pre` field is the operation-local pre-op backing of that overlay name
/// — the `Backing` the kernel resolved immediately before the op (see
/// [`Backing`]). It seeds the range-start `old` side during the fold (first touch
/// wins) and `diff` reads it for the old content, so status/diff need neither a
/// rebuilt previous tree nor a base stat (O(segment), not O(journal)). For an
/// already-staged file it is the staged inode (`StagedFile`), not the base it
/// was COW'd from; a pre the kernel could not resolve is `None`.
#[derive(Debug, Clone)]
pub enum Action {
    /// Stage (create or COW). The post-target is `StagedFile(ino)`; `pre` is the
    /// content this stage overwrote (`None` for a fresh create).
    Stage { path: String, ino: u32, pre: Backing },
    /// Delete. The post-target is `None`; `pre` is the removed content.
    Delete { path: String, pre: Backing },
    /// Rename. `src_pre`/`dst_pre` are the source's and destination's pre-op
    /// backings (the destination's is `None` for a fresh name or a tombstone).
    Rename {
        src: String,
        dst: String,
        src_pre: Backing,
        dst_pre: Backing,
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

/// How a prompted or denied access was resolved.
/// Journal encoding: `d` / `y` / `n`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateResult {
    DirectDeny,
    AskAllow,
    AskDeny,
}

impl GateResult {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            b'd' => Some(Self::DirectDeny),
            b'y' => Some(Self::AskAllow),
            b'n' => Some(Self::AskDeny),
            _ => None,
        }
    }
}

/// An explicit policy assignment recorded by C.
/// Journal encoding: `q` / `a` / `w` / `r` / `d` / `h` / `u`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    Ask,
    Allow,
    WriteAsk,
    ReadOnly,
    Deny,
    Hide,
    Unset,
}

impl Policy {
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            b'q' => Some(Self::Ask),
            b'a' => Some(Self::Allow),
            b'w' => Some(Self::WriteAsk),
            b'r' => Some(Self::ReadOnly),
            b'd' => Some(Self::Deny),
            b'h' => Some(Self::Hide),
            b'u' => Some(Self::Unset),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Allow => "allow",
            Self::WriteAsk => "write-ask",
            Self::ReadOnly => "read-only",
            Self::Deny => "deny",
            Self::Hide => "hide",
            Self::Unset => "unset",
        }
    }
}

/// An observational note — does not affect override state, only audit.
///
/// Emitted by the kernel and ignored by commit/abort/diff/replay; review
/// summaries and `yolo journal` surface them.
#[derive(Debug, Clone)]
pub enum Note {
    /// Prompted or statically denied access.
    /// Wire: `G\0<path>\0<op>\0<result>\n`.
    Gate {
        path: String,
        op: Op,
        result: GateResult,
    },
    /// Successful explicit policy assignment on a live mount.
    /// Wire: `C\0<path>\0<policy>\n`.
    Configure { path: String, policy: Policy },
}

/// A parsed journal record (interleaved actions, markers, and notes).
#[derive(Debug, Clone)]
pub enum Record {
    Action(Action),
    Marker(Marker),
    Note(Note),
}

/// A group of records (S/D/R + G/C) between consecutive P/T boundaries.
///
/// `Record::Marker` is never pushed into a segment by `Journal::new` —
/// markers split segments. Only `Record::Action` and `Record::Note`
/// appear here.
#[derive(Debug)]
pub struct Segment {
    /// The S/D/R + G/C records in this segment (no P/T records).
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

    #[test]
    fn gate_result_byte_roundtrips() {
        for (code, result) in [
            (b'd', GateResult::DirectDeny),
            (b'y', GateResult::AskAllow),
            (b'n', GateResult::AskDeny),
        ] {
            assert_eq!(GateResult::from_byte(code), Some(result));
        }
        assert_eq!(GateResult::from_byte(b'x'), None);
    }

    #[test]
    fn policy_codes_roundtrip() {
        for (code, policy) in [
            (b'q', Policy::Ask),
            (b'a', Policy::Allow),
            (b'w', Policy::WriteAsk),
            (b'r', Policy::ReadOnly),
            (b'd', Policy::Deny),
            (b'h', Policy::Hide),
            (b'u', Policy::Unset),
        ] {
            assert_eq!(Policy::from_byte(code), Some(policy));
        }
        assert_eq!(Policy::from_byte(b'x'), None);
    }
}
