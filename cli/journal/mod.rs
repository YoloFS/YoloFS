// agfs CLI — journal module
//
// Structured access to the append-only journal.
//
// Submodules:
//   types     — Action, Marker, Record, Segment
//   parse     — read(), parse()  (pub(super) only)
//   markers   — Markers (K/T skeleton: lookup, range, liveness computation)
//   journal   — Journal (segments + markers + precomputed liveness, borrowing filters)
//   tree      — DirTree builder: apply actions → dir tree, walk for display/restore

pub(crate) mod journal;
pub mod markers;
mod parse;
pub mod tree;
pub mod types;

pub use journal::Journal;
pub use markers::Markers;
pub use tree::{DirTree, Dstate};
pub use types::*;
