// agfs CLI — journal/types.rs
//
// Pure data types for journal records. No I/O dependencies.

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

    /// 2-bit encoding matching the kernel's `agfs_dtype_pack`.
    pub fn to_packed(&self) -> u64 {
        match self {
            DType::File => 0,
            DType::Dir => 1,
            DType::Link => 2,
        }
    }
}

/// A data mutation applied to the dir tree (A/M/D/R/P).
#[derive(Debug, Clone)]
pub enum Action {
    Add {
        path: String,
        dtype: Option<DType>,
        ino: u32,
    },
    Modify {
        path: String,
        dtype: Option<DType>,
        ino: u32,
    },
    Delete {
        path: String,
        dtype: Option<DType>,
    },
    Rename {
        src: String,
        dst: String,
        dtype: Option<DType>,
    },
    Replace {
        src: String,
        dst: String,
        dtype: Option<DType>,
    },
}

/// A control marker (K/T).
#[derive(Debug, Clone)]
pub enum Marker {
    Checkpoint { gen_id: u64, name: String },
    Restore { gen_id: u64, target_gen: u64 },
}

/// A parsed journal record (interleaved actions and markers).
#[derive(Debug, Clone)]
pub enum Record {
    Action(Action),
    Marker(Marker),
}

/// A group of data records (A/M/D/R/P) between consecutive K/T boundaries.
#[derive(Debug)]
pub struct Segment {
    /// The gen_id of the checkpoint this segment builds on.
    /// 0 for the 0-th segment (records before the first checkpoint).
    pub from: u64,
    /// The A/M/D/R/P records in this segment (no K/T records).
    pub records: Vec<Action>,
}
