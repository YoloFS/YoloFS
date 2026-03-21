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

/// A group of data records (A/M/D/R/P) between consecutive K/T boundaries.
#[derive(Debug)]
pub struct Segment {
    /// The gen_id of the checkpoint this segment builds on.
    /// 0 for the 0-th segment (records before the first checkpoint).
    pub from: u64,
    /// The A/M/D/R/P records in this segment (no K/T records).
    pub records: Vec<Record>,
}


/// A journal record: either an entry (dirent mutation) or a marker.
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
        dtype: Option<DType>,
    },
    Redirect {
        src: String,
        dst: String,
        dtype: Option<DType>,
    },
    Replace {
        src: String,
        dst: String,
        dtype: Option<DType>,
    },
    Checkpoint { gen_id: u64, name: String },
    Restore {
        gen_id: u64,
        target_gen: u64,
    },
}
