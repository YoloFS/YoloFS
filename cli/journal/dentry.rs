// agfs CLI — journal/dentry.rs
//
// Dentry: the state of a single entry in the overlay.

/// The target of a dentry — where content lives.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// Content staged in flat file store at this inode ID.
    Inode(u32),
    /// Redirect: content lives at a different base filesystem path.
    /// `None` = passthrough (identity / no staged change).
    /// `Some(src)` = redirect to `src`.
    Path(Option<String>),
    /// Content absent (hides base entry when in_base=true).
    None,
}

/// A dentry — the staged state of a single entry in the overlay.
///
/// Two orthogonal fields:
///   - `target`: where content lives (Inode, Path, or None).
///   - `in_base`: whether this path had content in the base filesystem
///     before staging.  Determines cleanup behavior: when removed or
///     moved away, `in_base=true` pins a negative dentry to hide the
///     base content; `in_base=false` cancels (just removes — nothing
///     in base to hide).
#[derive(Debug, Clone, PartialEq)]
pub struct Dentry {
    pub target: Target,
    pub in_base: bool,
}

impl Dentry {
    /// A passthrough dentry — represents no staged change.
    pub fn passthrough() -> Self {
        Dentry {
            target: Target::Path(None),
            in_base: true,
        }
    }

    /// True if this dentry is a passthrough (no staged change).
    pub fn is_passthrough(&self) -> bool {
        matches!(self.target, Target::Path(None))
    }

    /// Return the staged inode ID if this dentry carries one.
    pub fn ino(&self) -> Option<u32> {
        match self.target {
            Target::Inode(ino) => Some(ino),
            _ => None,
        }
    }

    /// True if this dentry involves the given path (as source or destination).
    pub fn matches_path(&self, dentry_path: &str, query: &str) -> bool {
        match &self.target {
            Target::Path(Some(src)) => dentry_path == query || src == query,
            _ => dentry_path == query,
        }
    }
}
