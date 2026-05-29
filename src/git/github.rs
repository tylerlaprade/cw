//! `gh` helpers. Auto-disabled when integration is off.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct PrInfo {
    pub state: String,
    pub head_branch: String,
}

/// One PR's identity + status, keyed by head branch in [`pr_map`].
#[derive(Debug, Clone)]
pub struct PrMeta {
    pub number: u32,
    pub state: String,
    pub is_draft: bool,
}

/// Fetch ALL of the repo's PRs in one call: head branch → {number, state,
/// isDraft}. Replaces per-workspace `pr_for_branch` + PR-state lookups in the
/// bulk paths (cleanup/teardown), matching the original's single
/// `gh pr list --state all --limit 500`. Empty map on any failure, so callers
/// can fall back to per-branch lookups (or treat "absent" as "no PR").
/// First PR seen per head wins (gh lists newest first — mirrors the old `.[0]`).
pub fn pr_map(inside: &Path) -> HashMap<String, PrMeta> {
    let mut map = HashMap::new();
    let out = Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "all",
            "--json",
            "number,headRefName,state,isDraft",
            "--limit",
            "500",
        ])
        .current_dir(inside)
        .output();
    let Ok(out) = out else { return map };
    if !out.status.success() {
        return map;
    }
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return map;
    };
    let Some(arr) = val.as_array() else {
        return map;
    };
    for pr in arr {
        let Some(branch) = pr.get("headRefName").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(number) = pr.get("number").and_then(|v| v.as_u64()) else {
            continue;
        };
        let state = pr
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let is_draft = pr.get("isDraft").and_then(|v| v.as_bool()).unwrap_or(false);
        map.entry(branch.to_string()).or_insert(PrMeta {
            number: number as u32,
            state,
            is_draft,
        });
    }
    map
}

/// Resolve a PR number to its head branch + state.
///
/// Uses `gh pr view <num> --json … -q '… | @tsv'` and reads the tab-separated
/// fields gh emits (no JSON parsing here).
pub fn view_pr(inside: &Path, number: u32) -> Result<PrInfo> {
    let out = Command::new("gh")
        .args([
            "pr",
            "view",
            &number.to_string(),
            "--json",
            "state,headRefName",
            "-q",
            "[.state, .headRefName] | @tsv",
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
    if fields.len() < 2 {
        anyhow::bail!("unexpected gh output: {line:?}");
    }
    Ok(PrInfo {
        state: fields[0].to_string(),
        head_branch: fields[1].to_string(),
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
