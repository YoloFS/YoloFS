// agfs CLI — journal module
//
// Structured access to the append-only journal.
//
// Submodules:
//   types     — Record, Change, Action, ActionList, DType, INO_REDIRECT
//   parse     — read()
//   markers   — Markers (CKP/RST skeleton: lookup, range computation)
//   segment   — Segment, SegmentedJournal (split records at CKP/RST boundaries)
//   liveness  — alive_segments(), live(), live_prefix(), live_slice() (reachability filtering)
//   compact   — compact() records into ActionList (decompose, cancel, merge)
//   action    — ActionList: apply() to base fs, collapse() to Changeset

pub mod action;
pub mod liveness;
pub mod markers;
pub mod parse;
pub mod segment;
pub mod compact;
pub mod types;

// Re-export types and parse so callers can write journal::Record, journal::read(), etc.
pub use markers::Markers;
pub use parse::*;
pub use segment::SegmentedJournal;
pub use types::*;
