// agfs CLI — journal module
//
// Structured access to the append-only journal.
//
// Submodules:
//   types     — Record, Change, Action, ActionList, DType, INO_REDIRECT
//   parse     — read()
//   segment   — Segment, Markers, SegmentedJournal (split records at K/S boundaries)
//   liveness  — alive_segments(), live(), live_prefix(), live_slice() (reachability filtering)
//   simplify  — simplify() records into ActionList (chain collapse, cancel, etc.)
//   action    — ActionList: apply() to base fs, collapse() to Changeset

pub mod action;
pub mod liveness;
pub mod parse;
pub mod segment;
pub mod simplify;
pub mod types;

// Re-export types and parse so callers can write journal::Record, journal::read(), etc.
pub use parse::*;
pub use segment::{Markers, SegmentedJournal};
pub use types::*;
