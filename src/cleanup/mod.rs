//! Full-sweep cleanup. Lands in step 8.

use crate::cli::CleanupArgs;
use crate::shell::Emitter;
use anyhow::Result;

pub fn run(_args: CleanupArgs, _emitter: &mut Emitter) -> Result<()> {
    Err(anyhow::anyhow!("`cw cleanup` lands in step 8"))
}
