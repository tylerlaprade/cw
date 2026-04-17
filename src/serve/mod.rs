//! Dev-server lifecycle manager. Lands in step 2.

pub mod logs;
pub mod processes;

use crate::cli::ServeArgs;
use crate::shell::Emitter;
use anyhow::Result;

pub fn run(_args: ServeArgs, _emitter: &mut Emitter) -> Result<()> {
    Err(anyhow::anyhow!("`cw serve` lands in step 2"))
}
