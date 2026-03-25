// agfs CLI — journal/types.rs
//
// Pure data types for journal records. No I/O dependencies.

/// Validate a raw d_type value (libc DT_* constant).
pub fn dtype_valid(val: u8) -> bool {
    matches!(
        val,
        libc::DT_REG
            | libc::DT_DIR
            | libc::DT_LNK
            | libc::DT_BLK
            | libc::DT_CHR
            | libc::DT_FIFO
            | libc::DT_SOCK
    )
}

/// The target of a dentry — where content lives.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// Content staged in flat file store at this inode ID.
    Inode(u32),
    /// Redirect: content lives at a different base filesystem path.
    /// `None` = passthrough (identity / no staged change).
    /// `Some(src)` = redirect to `src`.
    Path(Option<String>),
    /// Content absent (tombstone).
    None,
}

impl Target {
    /// A passthrough target — represents no staged change.
    pub fn passthrough() -> Self {
        Target::Path(None)
    }

    /// True if this target is a passthrough (no staged change).
    pub fn is_passthrough(&self) -> bool {
        matches!(self, Target::Path(None))
    }

    /// Return the staged inode ID if this target carries one.
    pub fn ino(&self) -> Option<u32> {
        match self {
            Target::Inode(ino) => Some(*ino),
            _ => None,
        }
    }

    /// True if this target involves the given path (as source or destination).
    pub fn matches_path(&self, dentry_path: &str, query: &str) -> bool {
        match self {
            Target::Path(Some(src)) => dentry_path == query || src == query,
            _ => dentry_path == query,
        }
    }
}

/// A data mutation applied to the dir tree (A/D/R).
#[derive(Debug, Clone)]
pub enum Action {
    Add {
        path: String,
        dtype: Option<u8>,
        ino: u32,
    },
    Delete {
        path: String,
        dtype: Option<u8>,
    },
    Rename {
        src: String,
        dst: String,
        dtype: Option<u8>,
    },
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

/// A group of data records (A/D/R) between consecutive M/J boundaries.
#[derive(Debug)]
pub struct Segment {
    /// The gen_id of the mark this segment builds on.
    /// 0 for the 0-th segment (records before the first mark).
    pub from: u64,
    /// The A/D/R records in this segment (no M/J records).
    pub records: Vec<Action>,
}
