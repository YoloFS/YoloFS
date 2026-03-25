// agfs CLI — journal module
//
// Structured access to the append-only journal.
//
// Submodules:
//   types     — Action, Meta, Record, Segment
//   parse     — read(), parse()  (pub(super) only)
//   metas     — Metas (M/J skeleton: lookup, range, liveness computation)
//   journal   — Journal (segments + metas + precomputed liveness, borrowing filters)
//   tree      — DirTree builder: apply actions → dir tree, walk for display/jump
pub(crate) mod core;

pub mod metas;
mod parse;
pub mod tree;
pub mod types;

pub use self::core::Journal;
pub use metas::Metas;
pub use tree::{DirNode, DirTree};
pub use types::*;
