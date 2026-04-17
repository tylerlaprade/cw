//! Daemonize a command: double-fork, setsid, redirect stdio to a log file,
//! write a SETUP_DONE sentinel on clean exit. Used by `cw serve start` and
//! the workspace post-create setup pipeline.
//!
//! Landing in step 2. For now, a placeholder that delegates to nohup.

use anyhow::Result;

pub fn spawn_detached(_argv: &[&str], _log: &std::path::Path) -> Result<u32> {
    Err(anyhow::anyhow!("spawn_detached lands in step 2"))
}
