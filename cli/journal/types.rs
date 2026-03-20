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
/// first element of `(String, Change)` tuples returned by `resolve()`.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Added { ino: u64, dtype: DType },
    Modified { ino: u64, dtype: DType },
    Deleted,
    Renamed { from: String, dtype: DType },
    Replaced { from: String, dtype: DType },
}

impl Change {
    /// Return the staged inode ID if this change carries one.
    pub fn ino(&self) -> Option<u64> {
        match self {
            Change::Added { ino, .. } | Change::Modified { ino, .. } => Some(*ino),
            _ => None,
        }
    }

    /// True if this change involves the given path (as source or destination).
    pub fn matches_path(&self, path: &str, target: &str) -> bool {
        match self {
            Change::Added { .. } | Change::Modified { .. } | Change::Deleted => path == target,
            Change::Renamed { from, .. } | Change::Replaced { from, .. } => {
                path == target || from == target
            }
        }
    }
}

/// A simplified journal action — only the 5 operation variants (no K/S).
/// Used by the simplify → apply/collapse pipeline.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Add {
        path: String,
        ino: u64,
        dtype: DType,
    },
    Modify {
        path: String,
        ino: u64,
        dtype: DType,
    },
    Delete {
        path: String,
    },
    Rename {
        old: String,
        new: String,
        dtype: DType,
    },
    Replace {
        old: String,
        new: String,
        dtype: DType,
    },
}

/// An ordered sequence of simplified actions — directly replayable on
/// the base filesystem.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionList(pub Vec<Action>);

/// A parsed journal — a flat list of records from disk.
#[derive(Debug, Clone, Default)]
pub struct RawJournal(pub Vec<Record>);

/// The resolved final state — a list of (path, change) pairs.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Changeset(pub Vec<(String, Change)>);

/// Live segments — only those that survive restore pruning (Level 2).
#[derive(Debug, Default)]
pub struct LiveSegments(pub Vec<Segment>);

impl LiveSegments {
    /// Flatten into a single record list.
    pub fn into_records(self) -> Vec<Record> {
        self.0.into_iter().flat_map(|s| s.records).collect()
    }
}

#[cfg(test)]
mod live_segments_tests {
    use super::*;

    #[test]
    fn into_records_empty() {
        let ls = LiveSegments(vec![]);
        assert!(ls.into_records().is_empty());
    }

    #[test]
    fn into_records_flattens_segments() {
        let ls = LiveSegments(vec![
            Segment {
                from: 0,
                records: vec![Record::Added {
                    path: "/a".into(),
                    ino: 10,
                    dtype: DType::File.into(),
                }],
            },
            Segment {
                from: 1,
                records: vec![],
            },
            Segment {
                from: 2,
                records: vec![
                    Record::Added {
                        path: "/b".into(),
                        ino: 20,
                        dtype: DType::File.into(),
                    },
                    Record::Deleted {
                        path: "/c".into(),
                    },
                ],
            },
        ]);
        let records = ls.into_records();
        assert_eq!(records.len(), 3);
        assert!(matches!(&records[0], Record::Added { path, .. } if path == "/a"));
        assert!(matches!(&records[1], Record::Added { path, .. } if path == "/b"));
        assert!(matches!(&records[2], Record::Deleted { path, .. } if path == "/c"));
    }
}

/// A group of data records (A/M/D/R) between consecutive K/S boundaries.
#[derive(Debug)]
pub struct Segment {
    /// The gen_id of the checkpoint this segment builds on.
    /// 0 for the 0-th segment (records before the first checkpoint).
    pub from: u64,
    /// The A/M/D/R records in this segment (no K/S records).
    pub records: Vec<Record>,
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
        old: String,
        new: String,
        dtype: Option<DType>,
    },
    Replace {
        old: String,
        new: String,
        dtype: Option<DType>,
    },
    Checkpoint { gen_id: u64, name: String },
    Restore {
        gen_id: u64,
        target_gen: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Change::ino() ────────────────────────────────────────────────

    #[test]
    fn ino_added() {
        let d = Change::Added {
            ino: 42,
            dtype: DType::File,
        };
        assert_eq!(d.ino(), Some(42));
    }

    #[test]
    fn ino_modified() {
        let d = Change::Modified {
            ino: 7,
            dtype: DType::Dir,
        };
        assert_eq!(d.ino(), Some(7));
    }

    #[test]
    fn ino_deleted_is_none() {
        assert_eq!(Change::Deleted.ino(), None);
    }

    #[test]
    fn ino_renamed_is_none() {
        let d = Change::Renamed {
            from: "/old".into(),
            dtype: DType::File,
        };
        assert_eq!(d.ino(), None);
    }

    #[test]
    fn ino_replaced_is_none() {
        let d = Change::Replaced {
            from: "/old".into(),
            dtype: DType::File,
        };
        assert_eq!(d.ino(), None);
    }

    // ── Change::matches_path() ───────────────────────────────────────

    #[test]
    fn matches_path_added() {
        let d = Change::Added {
            ino: 1,
            dtype: DType::File,
        };
        assert!(d.matches_path("/a", "/a"));
        assert!(!d.matches_path("/a", "/b"));
    }

    #[test]
    fn matches_path_modified() {
        let d = Change::Modified {
            ino: 1,
            dtype: DType::File,
        };
        assert!(d.matches_path("/x", "/x"));
        assert!(!d.matches_path("/x", "/y"));
    }

    #[test]
    fn matches_path_deleted() {
        assert!(Change::Deleted.matches_path("/a", "/a"));
        assert!(!Change::Deleted.matches_path("/a", "/b"));
    }

    #[test]
    fn matches_path_renamed_checks_both() {
        let d = Change::Renamed {
            from: "/old".into(),
            dtype: DType::File,
        };
        assert!(d.matches_path("/new", "/new"), "matches destination");
        assert!(d.matches_path("/new", "/old"), "matches source");
        assert!(!d.matches_path("/new", "/other"));
    }

    #[test]
    fn matches_path_replaced_checks_both() {
        let d = Change::Replaced {
            from: "/old".into(),
            dtype: DType::File,
        };
        assert!(d.matches_path("/new", "/new"), "matches destination");
        assert!(d.matches_path("/new", "/old"), "matches source");
        assert!(!d.matches_path("/new", "/other"));
    }
}
