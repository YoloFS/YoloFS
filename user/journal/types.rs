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

/// A control meta (M/J).
#[derive(Debug, Clone)]
pub enum Meta {
    Mark { gen_id: u64, name: String },
    Jump { gen_id: u64, target_gen: u64 },
}

/// A parsed journal record (interleaved actions and metas).
#[derive(Debug, Clone)]
pub enum Record {
    Action(Action),
    Meta(Meta),
}

/// A group of data records (S/D/R) between consecutive M/J boundaries.
#[derive(Debug)]
pub struct Segment {
    /// The gen_id of the mark this segment builds on.
    /// 0 for the 0-th segment (records before the first mark).
    pub from: u64,
    /// The S/D/R records in this segment (no M/J records).
    pub records: Vec<Action>,
}
