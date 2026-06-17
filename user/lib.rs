pub mod changeset;
pub mod cmd;

/// Subcommands an agent may run via its hook's `yolo run -- yolo <sub>` — read
/// and navigation only. Everything else (commit/abort/rule/session control/…)
/// is the human's, so the gate in `main.rs` is default-deny against this list.
/// `yolo init`'s scaffolded agent guide (`user/templates/agent-guide.md`)
/// documents exactly this set, so the two cannot drift.
pub const AGENT_ALLOWED: &[&str] = &["review", "journal", "timeline", "travel", "snapshot"];

pub mod config;
pub mod ioctl;
pub mod journal;
pub mod kmsg;
pub mod perm;
pub mod report;
pub mod utils;
