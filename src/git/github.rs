//! `gh` helpers. Auto-disabled when integration is off.

use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct PrInfo {
    pub number: u32,
    pub state: String,
    pub head_branch: String,
    pub base_branch: String,
}

/// Resolve a PR number to its head branch + state.
///
/// Uses `gh pr view <num> --json state,headRefName,baseRefName` and parses
/// the JSON out by hand (to avoid pulling in serde_json).
pub fn view_pr(inside: &Path, number: u32) -> Result<PrInfo> {
    let out = Command::new("gh")
        .args([
            "pr",
            "view",
            &number.to_string(),
            "--json",
            "state,headRefName,baseRefName",
            "-q",
            "[.state, .headRefName, .baseRefName] | @tsv",
        ])
        .current_dir(inside)
        .output()
        .with_context(|| format!("gh pr view {number}"))?;
    if !out.status.success() {
        anyhow::bail!(
            "gh pr view {} failed: {}",
            number,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let stdout = String::from_utf8(out.stdout)?;
    let line = stdout.trim();
    let fields: Vec<&str> = line.split('\t').collect();
    if fields.len() < 3 {
        anyhow::bail!("unexpected gh output: {line:?}");
    }
    Ok(PrInfo {
        number,
        state: fields[0].to_string(),
        head_branch: fields[1].to_string(),
        base_branch: fields[2].to_string(),
    })
}

/// Find the PR number whose head branch matches `branch`. Returns None when
/// no PR exists or `gh` isn't available.
pub fn pr_for_branch(inside: &Path, branch: &str) -> Option<u32> {
    let out = Command::new("gh")
        .args([
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "all",
            "--json",
            "number",
            "-q",
            ".[0].number",
        ])
        .current_dir(inside)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    s.parse::<u32>().ok()
}
