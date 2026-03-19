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
    Restore { gen_id: u64, target_gen: u64 },
}
