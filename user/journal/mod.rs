// yolo CLI — journal module
//
// Structured access to the append-only journal.
//
// Submodules:
//   types     — Action, Meta, Note, Record, Segment
//   parse     — read(), parse()  (pub(super) only)
//   meta      — MetaIndex (P/T skeleton: lookup, range, liveness computation)
//   journal   — Journal (segments + metas + precomputed liveness, borrowing filters)
//   tree      — DirTree builder: apply actions → dir tree, walk for display/travel
//   plan      — DirTree → Actions: commit plan (inverse of tree)
pub(crate) mod core;

pub mod meta;
mod parse;
mod plan;
pub mod tree;
pub mod types;

pub use self::core::Journal;
pub use meta::MetaIndex;
pub use plan::CommitPlan;
pub use tree::{DirNode, DirTree};
pub use types::*;
