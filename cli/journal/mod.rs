// agfs CLI — journal module
//
// Structured access to the append-only journal.
//
// Submodules:
//   types     — Record, Checkpoint, DType, INO_REDIRECT
//   parse     — read(), inode_path(), truncate()
//   timeline  — Timeline, Segment, reachable(), find_checkpoint_index(), slice_records()
//   resolve   — Resolver, Change, ResolvedSegment, resolve(), resolve_segments()

pub mod types;
pub mod parse;
pub mod timeline;
pub mod resolve;

// Re-export types and parse so callers can write journal::Record, journal::read(), etc.
pub use types::*;
pub use parse::*;
