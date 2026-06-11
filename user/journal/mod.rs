// yolo CLI — journal module
//
// Structured access to the append-only journal.
//
// Submodules:
//   types     — Action, Marker, Note, Record, Segment
//   parse     — read(), parse()  (pub(super) only)
//   marker      — MarkerIndex (P/T skeleton: lookup, range, liveness computation)
//   journal   — Journal (segments + markers + precomputed liveness, borrowing filters)
//   tree      — DirTree builder: apply actions → dir tree, walk for display/travel
//   plan      — DirTree → commit plan of `CommitOp`s (inverse of tree)
pub(crate) mod core;

pub mod marker;
mod parse;
mod plan;
pub mod tree;
pub mod types;

pub use self::core::Journal;
pub use marker::MarkerIndex;
pub use plan::{CommitOp, CommitPlan};
pub use tree::{DirNode, DirTree};
pub use types::*;
