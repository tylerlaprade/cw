//! PR fetching via `gh`: list, required-check contexts, and the GraphQL
//! review-feedback query. Raw JSON is deserialized with serde; the
//! actionability classification lives in `super::actionability`.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

/// A single check run from `statusCheckRollup`. StatusContext-shaped entries
/// (which have `context`/`state` instead of `name`/`conclusion`) deserialize
/// to an empty name + no conclusion and are ignored — matching the original.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub details_url: Option<String>,
}

/// A requested reviewer — a user (`login`) or a team (`name`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRequest {
    #[serde(default)]
    pub login: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

impl ReviewRequest {
    pub fn id(&self) -> Option<String> {
        self.login.clone().or_else(|| self.name.clone())
    }
}

/// A PR as returned by `gh pr list --json ...`. Field names match gh's JSON
/// (camelCase) so serde can deserialize directly. Only the fields the
/// classification needs are modeled; gh returns the rest and serde ignores it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pr {
    pub number: u32,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub base_ref_name: String,
    #[serde(default)]
    pub mergeable: String,
    #[serde(default)]
    pub review_decision: String,
    #[serde(default)]
    pub review_requests: Vec<ReviewRequest>,
    /// Present (non-null) when the PR is queued for auto-merge.
    #[serde(default)]
    pub auto_merge_request: Option<serde_json::Value>,
    #[serde(default)]
    pub is_draft: bool,
    #[serde(default)]
    pub status_check_rollup: Vec<Check>,
}

const PR_FIELDS: &str = "number,title,baseRefName,mergeable,reviewDecision,reviewRequests,autoMergeRequest,isDraft,statusCheckRollup";

/// List my open PRs whose base is `base` and that aren't drafts.
pub fn list_my_open_prs(inside: &Path, base: &str) -> Result<Vec<Pr>> {
    let out = Command::new("gh")
        .args([
            "pr", "list", "--author", "@me", "--state", "open", "--limit", "100", "--json",
            PR_FIELDS,
        ])
        .current_dir(inside)
        .output()
        .context("gh pr list")?;
    if !out.status.success() {
        anyhow::bail!(
            "gh pr list failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let all: Vec<Pr> = serde_json::from_slice(&out.stdout).context("parsing gh pr list JSON")?;
    Ok(all
        .into_iter()
        .filter(|p| p.base_ref_name == base && !p.is_draft)
        .collect())
}

/// `owner/repo` for the current repository, via `gh repo view`.
pub fn repo_owner_name(inside: &Path) -> Option<(String, String)> {
    let out = Command::new("gh")
        .args([
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "-q",
            ".nameWithOwner",
        ])
        .current_dir(inside)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim();
    let (owner, name) = s.split_once('/')?;
    Some((owner.to_string(), name.to_string()))
}

/// Required-status-check contexts for `base` via branch protection. Returns an
/// empty set when protection is absent or inaccessible (common: not an admin).
pub fn fetch_required_checks(
    inside: &Path,
    owner: &str,
    repo: &str,
    base: &str,
) -> HashSet<String> {
    let path = format!("repos/{owner}/{repo}/branches/{base}/protection/required_status_checks");
    let out = Command::new("gh")
        .args(["api", &path, "-q", "[.checks[].context] | join(\",\")"])
        .current_dir(inside)
        .output();
    let Ok(out) = out else {
        return HashSet::new();
    };
    if !out.status.success() {
        return HashSet::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// Build the GraphQL query fetching review feedback for each PR, aliased as
/// `pr<number>`. Mirrors format-triage.py's `--query` (non-verbose) mode.
fn feedback_query(owner: &str, repo: &str, numbers: &[u32]) -> String {
    let parts: Vec<String> = numbers
        .iter()
        .map(|n| {
            format!(
                "pr{n}: repository(owner: \"{owner}\", name: \"{repo}\") {{ \
                 pullRequest(number: {n}) {{ \
                 author {{ login }} \
                 reviewThreads(first: 100) {{ nodes {{ isResolved isOutdated }} }} \
                 reviews(first: 100) {{ nodes {{ state body author {{ login }} createdAt }} }} \
                 comments(first: 100) {{ nodes {{ author {{ login }} createdAt }} }} \
                 timelineItems(itemTypes: [REVIEW_DISMISSED_EVENT], first: 100) {{ nodes {{ ... on ReviewDismissedEvent {{ createdAt actor {{ login }} }} }} }} \
                 }} }}"
            )
        })
        .collect();
    format!("{{ {} }}", parts.join(" "))
}

/// Fetch the review-feedback payload for `numbers`. Returns the raw `gh api
/// graphql` JSON response (navigated by `super::actionability`). Empty object
/// on any failure.
pub fn fetch_feedback(
    inside: &Path,
    owner: &str,
    repo: &str,
    numbers: &[u32],
) -> serde_json::Value {
    if numbers.is_empty() {
        return serde_json::json!({});
    }
    let query = feedback_query(owner, repo, numbers);
    let out = Command::new("gh")
        .args(["api", "graphql", "-f"])
        .arg(format!("query={query}"))
        .current_dir(inside)
        .output();
    let Ok(out) = out else {
        return serde_json::json!({});
    };
    if !out.status.success() {
        return serde_json::json!({});
    }
    serde_json::from_slice(&out.stdout).unwrap_or_else(|_| serde_json::json!({}))
}
