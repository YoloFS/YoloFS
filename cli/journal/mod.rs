// agfs CLI — journal module
//
// Structured access to the append-only journal.
//
// Submodules:
//   types     — Record, Dirent, Checkpoint, DType, INO_REDIRECT
//   parse     — read(), inode_path(), truncate()
//   timeline  — Timeline, Segment; flat helpers (pub(crate)): reachable(), find_checkpoint_index(), slice_records()
//   resolve   — Resolver, ResolvedSegment, resolve(), resolve_segments()

pub mod parse;
pub mod resolve;
pub mod timeline;
pub mod types;

// Re-export types and parse so callers can write journal::Record, journal::read(), etc.
pub use parse::*;
pub use types::*;
