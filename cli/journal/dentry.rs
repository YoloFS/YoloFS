// agfs CLI — journal/dentry.rs
//
// Dentry: the state of a single entry in the overlay.

/// A dentry — the state of a single entry in the overlay.
///
/// `in_base` indicates whether this path position had content in the base
/// filesystem before staging.  It determines cleanup behavior: when removed
/// or moved away, `in_base=true` leaves a Tombstone to hide the base content;
/// `in_base=false` cancels (just removes — nothing in base to hide).
#[derive(Debug, Clone, PartialEq)]
pub enum Dentry {
    StagedInode {
        ino: u32,
        dtype: u8,
        in_base: bool,
    },
    Redirect {
        /// The source path in the base filesystem (where the content lives).
        src: String,
        dtype: u8,
        in_base: bool,
    },
    Tombstone {
        dtype: u8,
    },
    Unset,
}

impl Dentry {
    pub fn dtype(&self) -> u8 {
        match self {
            Dentry::StagedInode { dtype, .. }
            | Dentry::Redirect { dtype, .. }
            | Dentry::Tombstone { dtype } => *dtype,
            Dentry::Unset => unreachable!("Unset has no dtype"),
        }
    }

    pub fn in_base(&self) -> bool {
        match self {
            Dentry::StagedInode { in_base, .. } | Dentry::Redirect { in_base, .. } => *in_base,
            Dentry::Tombstone { .. } | Dentry::Unset => true,
        }
    }

    /// Return the staged inode ID if this dentry carries one.
    pub fn ino(&self) -> Option<u32> {
        match self {
            Dentry::StagedInode { ino, .. } => Some(*ino),
            _ => None,
        }
    }

    /// True if this dentry involves the given path (as source or destination).
    pub fn matches_path(&self, dentry_path: &str, query: &str) -> bool {
        match self {
            Dentry::StagedInode { .. } | Dentry::Tombstone { .. } | Dentry::Unset => {
                dentry_path == query
            }
            Dentry::Redirect { src, .. } => dentry_path == query || src == query,
        }
    }

    pub(super) fn set_in_base(&mut self, val: bool) {
        match self {
            Dentry::StagedInode { in_base, .. } | Dentry::Redirect { in_base, .. } => {
                *in_base = val
            }
            Dentry::Tombstone { .. } | Dentry::Unset => {}
        }
    }
}
