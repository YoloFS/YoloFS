// agfs CLI — journal/types.rs
//
// Pure data types for journal records. No I/O dependencies.

pub const INO_REDIRECT: u64 = u64::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DType {
    File,
    Dir,
    Link,
}

impl DType {
    pub fn from_char(c: u8) -> Option<DType> {
        match c {
            b'f' => Some(DType::File),
            b'd' => Some(DType::Dir),
            b'l' => Some(DType::Link),
            _ => None,
        }
    }

    pub fn to_libc(&self) -> u8 {
        match self {
            DType::File => libc::DT_REG,
            DType::Dir => libc::DT_DIR,
            DType::Link => libc::DT_LNK,
        }
    }
}

/// A resolved change — the final effect of replaying the journal.
/// Path-free: the primary path (destination) is carried externally as the
/// first element of `(String, Dirent)` tuples returned by `resolve()`.
#[derive(Clone, Debug, PartialEq)]
pub enum Dirent {
    Added { ino: u64, dtype: DType },
    Modified { ino: u64, dtype: DType },
    Deleted,
    Renamed { from: String, dtype: DType },
    Replaced { from: String, dtype: DType },
}

impl Dirent {
    /// Return the staged inode ID if this change carries one.
    pub fn ino(&self) -> Option<u64> {
        match self {
            Dirent::Added { ino, .. } | Dirent::Modified { ino, .. } => Some(*ino),
            _ => None,
        }
    }

    /// True if this change involves the given path (as source or destination).
    pub fn matches_path(&self, path: &str, target: &str) -> bool {
        match self {
            Dirent::Added { .. } | Dirent::Modified { .. } | Dirent::Deleted => path == target,
            Dirent::Renamed { from, .. } | Dirent::Replaced { from, .. } => {
                path == target || from == target
            }
        }
    }
}

/// A named checkpoint in the journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub gen_id: u64,
    pub name: String,
}

/// A journal record: either an entry (dirent mutation) or a checkpoint.
#[derive(Debug, Clone)]
pub enum Record {
    Added {
        path: String,
        dtype: Option<DType>,
        ino: u64,
    },
    Modified {
        path: String,
        dtype: Option<DType>,
        ino: u64,
    },
    Deleted {
        path: String,
    },
    Redirect {
        path: String,
        dtype: Option<DType>,
        base: String,
    },
    Replace {
        path: String,
        dtype: Option<DType>,
        base: String,
    },
    Checkpoint(Checkpoint),
    Restore {
        gen_id: u64,
        target_gen: u64,
    },
}
