//! PR fetching via `gh`.

use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct Pr {
    pub number: u32,
    pub title: String,
    pub base: String,
    pub merge_state: String,
    pub review_decision: String,
    pub is_draft: bool,
    pub failing_checks: Vec<String>,
}

pub fn list_my_open_prs(inside: &Path, base: &str) -> Result<Vec<Pr>> {
    // Use -q to coerce JSON to TSV. One row per PR, pipe-separated check list.
    let q = r#"[ .[]
      | select(.baseRefName == $base)
      | select(.isDraft == false)
      | [
          (.number|tostring),
          .title,
          .baseRefName,
          (.mergeStateStatus // ""),
          (.reviewDecision // ""),
          (if .isDraft then "1" else "0" end),
          ([ .statusCheckRollup[]? | select((.conclusion // .status) == "FAILURE") | .name ] | join("|"))
        ]
      | @tsv
    ] | .[]"#;
    let out = Command::new("gh")
        .args([
            "pr",
            "list",
            "--author",
            "@me",
            "--state",
            "open",
            "--limit",
            "100",
            "--json",
            "number,title,baseRefName,mergeStateStatus,isDraft,reviewDecision,statusCheckRollup",
            "--jq",
        ])
        .arg(q.replace("$base", &format!("\"{base}\"")))
        .current_dir(inside)
        .output()
        .context("gh pr list")?;
    if !out.status.success() {
        anyhow::bail!(
            "gh pr list failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let mut prs = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 7 {
            continue;
        }
        prs.push(Pr {
            number: f[0].parse().unwrap_or(0),
            title: f[1].to_string(),
            base: f[2].to_string(),
            merge_state: f[3].to_string(),
            review_decision: f[4].to_string(),
            is_draft: f[5] == "1",
            failing_checks: if f[6].is_empty() {
                Vec::new()
            } else {
                f[6].split('|').map(String::from).collect()
            },
        });
    }
    Ok(prs)
}
