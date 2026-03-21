// agfs CLI — journal module
//
// Structured access to the append-only journal.
//
// Submodules:
//   types     — Record, DType, INO_REDIRECT
//   parse     — read()
//   markers   — Markers (K/T skeleton: lookup, range computation)
//   segment   — Segment, SegmentedJournal (split records at K/T boundaries)
//   liveness  — alive_segments(), live(), live_prefix(), live_slice() (reachability filtering)
//   tree      — DirTree builder: apply records → dir tree, walk for display/restore

pub mod liveness;
pub mod markers;
pub mod parse;
pub mod segment;
pub mod tree;
pub mod types;

// Re-export types and parse so callers can write journal::Record, journal::read(), etc.
pub use markers::Markers;
pub use parse::*;
pub use segment::SegmentedJournal;
pub use tree::{DirTree, Dirent};
pub use types::*;
