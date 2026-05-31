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
    Stage { path: String, ino: u32 },
    Delete { path: String },
    Rename { src: String, dst: String },
}

/// A control marker (P/T).
#[derive(Debug, Clone)]
pub enum Marker {
    Snapshot { gen_id: u64, name: String },
    Travel { gen_id: u64, target_gen: u64 },
}

/// An observational note — does not affect overlay state, only audit.
///
/// `Note::Block` records that a yolofs rule returned `-EACCES` for the
/// given path. The kernel emits these via `B\0<path>\n` in the journal.
#[derive(Debug, Clone)]
pub enum Note {
    Block { path: String },
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
