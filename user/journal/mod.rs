// agfs CLI — journal module
//
// Structured access to the append-only journal.
//
// Submodules:
//   types     — Action, Meta, Record, Segment
//   parse     — read(), parse()  (pub(super) only)
//   meta      — MetaIndex (M/J skeleton: lookup, range, liveness computation)
//   journal   — Journal (segments + metas + precomputed liveness, borrowing filters)
//   tree      — DirTree builder: apply actions → dir tree, walk for display/jump
pub(crate) mod core;

pub mod meta;
mod parse;
pub mod tree;
pub mod types;

pub use self::core::Journal;
pub use meta::MetaIndex;
pub use tree::{DirNode, DirTree};
pub use types::*;
