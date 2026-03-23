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
pub(crate) mod core;
pub mod dstate;

pub mod markers;
mod parse;
pub mod tree;
pub mod types;

pub use self::core::Journal;
pub use dstate::Dstate;
pub use markers::Markers;
pub use tree::DirTree;
pub use types::*;
