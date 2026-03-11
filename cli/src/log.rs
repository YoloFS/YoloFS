// agfs CLI — log.rs
//
// `agfs log` — not yet implemented with ioctl interface.

use anyhow::Result;

pub fn run(_follow: bool, _dump: bool) -> Result<()> {
    anyhow::bail!("log is not yet implemented with the ioctl interface");
}
