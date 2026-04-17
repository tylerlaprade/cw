//! Actionable-work dashboard. Lands in step 9.

pub mod actions;
pub mod gh;
pub mod jira;
pub mod render;

use crate::cli::TriageArgs;
use anyhow::Result;

pub fn run(_args: TriageArgs) -> Result<()> {
    Err(anyhow::anyhow!("`cw triage` lands in step 9"))
}
