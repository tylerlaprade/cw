//! Rebase loop + resolver dispatch. Lands in step 6.

pub mod resolvers;

use crate::cli::RestackArgs;
use crate::shell::Emitter;
use anyhow::Result;

pub fn run(_args: RestackArgs, _emitter: &mut Emitter) -> Result<()> {
    Err(anyhow::anyhow!("`cw restack` lands in step 6"))
}
