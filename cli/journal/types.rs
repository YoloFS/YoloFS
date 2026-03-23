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

/// A data mutation applied to the dir tree (A/M/D/R/P).
#[derive(Debug, Clone)]
pub enum Action {
    Add {
        path: String,
        dtype: Option<u8>,
        ino: u32,
    },
    Modify {
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
    Replace {
        src: String,
        dst: String,
        dtype: Option<u8>,
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
