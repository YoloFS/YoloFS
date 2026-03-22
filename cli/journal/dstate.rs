// agfs CLI — journal/dstate.rs
//
// Dstate: the state of a single entry in the overlay.

/// A dstate — the state of a single entry in the overlay.
///
/// `in_base` indicates whether this path position had content in the base
/// filesystem before staging.  It determines cleanup behavior: when removed
/// or moved away, `in_base=true` leaves a Tombstone to hide the base content;
/// `in_base=false` cancels (just removes — nothing in base to hide).
#[derive(Debug, Clone, PartialEq)]
pub enum Dstate {
    StagedInode {
        ino: u32,
        dtype: u8,
        in_base: bool,
    },
    BasePath {
        /// The source path in the base filesystem (where the content lives).
        src: String,
        dtype: u8,
        in_base: bool,
    },
    Tombstone {
        dtype: u8,
    },
    Passthrough,
}

impl Dstate {
    pub fn dtype(&self) -> u8 {
        match self {
            Dstate::StagedInode { dtype, .. }
            | Dstate::BasePath { dtype, .. }
            | Dstate::Tombstone { dtype } => *dtype,
            Dstate::Passthrough => unreachable!("Passthrough has no dtype"),
        }
    }

    pub fn in_base(&self) -> bool {
        match self {
            Dstate::StagedInode { in_base, .. } | Dstate::BasePath { in_base, .. } => *in_base,
            Dstate::Tombstone { .. } | Dstate::Passthrough => true,
        }
    }

    /// Return the staged inode ID if this dstate carries one.
    pub fn ino(&self) -> Option<u32> {
        match self {
            Dstate::StagedInode { ino, .. } => Some(*ino),
            _ => None,
        }
    }

    /// True if this dstate involves the given path (as source or destination).
    pub fn matches_path(&self, dstate_path: &str, query: &str) -> bool {
        match self {
            Dstate::StagedInode { .. } | Dstate::Tombstone { .. } | Dstate::Passthrough => {
                dstate_path == query
            }
            Dstate::BasePath { src, .. } => dstate_path == query || src == query,
        }
    }

    pub(super) fn set_in_base(&mut self, val: bool) {
        match self {
            Dstate::StagedInode { in_base, .. } | Dstate::BasePath { in_base, .. } => *in_base = val,
            Dstate::Tombstone { .. } | Dstate::Passthrough => {}
        }
    }
}
