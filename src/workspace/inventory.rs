//! Enumerate existing workspaces + their status.

use crate::config::Config;
use crate::git::worktree;
use crate::util::paths;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct Entry {
    pub number: Option<u32>,
    pub dir: PathBuf,
    pub branch: Option<String>,
    pub merged: bool,
    pub remote_gone: bool,
    pub detached: bool,
    pub no_unique_commits: bool,
    pub pr_closed_or_merged: Option<u32>,
}

impl Entry {
    pub fn is_removable(&self) -> bool {
        self.detached
            || self.merged
            || self.remote_gone
            || self.no_unique_commits
            || self.pr_closed_or_merged.is_some()
    }
}

pub fn list_workspaces(cfg: &Config) -> Result<Vec<Entry>> {
    let Some(root) = cfg.runtime.repo_root.as_deref() else {
        return Ok(Vec::new());
    };
    let wts = worktree::list(root)?;
    let mut out = Vec::new();
    for w in wts {
        let number = paths::detect_number(&w.dir, &cfg.runtime.stem);
        let branch = w.branch_name().map(|s| s.to_string());
        let mut e = Entry {
            number,
            dir: w.dir.clone(),
            branch: branch.clone(),
            merged: false,
            remote_gone: false,
            detached: branch.is_none(),
            no_unique_commits: false,
            pr_closed_or_merged: None,
        };
        if let Some(b) = &branch {
            e.merged = is_merged(root, b, &cfg.runtime.base_branch);
            e.remote_gone = remote_gone(root, b);
            e.no_unique_commits = !has_unique_commits(&w.dir, b, &cfg.runtime.base_branch);
            if let Some(pr) = crate::git::github::pr_for_branch(&w.dir, b) {
                if let Some(state) = pr_state(&w.dir, pr) {
                    if matches!(state.as_str(), "MERGED" | "CLOSED" | "merged" | "closed") {
                        e.pr_closed_or_merged = Some(pr);
                    }
                }
            }
        }
        out.push(e);
    }
    Ok(out)
}

fn is_merged(inside: &Path, branch: &str, base: &str) -> bool {
    Command::new("git")
        .args(["merge-base", "--is-ancestor", branch, base])
        .current_dir(inside)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn remote_gone(inside: &Path, branch: &str) -> bool {
    let out = Command::new("git")
        .args(["for-each-ref", "--format=%(upstream:track)"])
        .arg(format!("refs/heads/{}", branch))
        .current_dir(inside)
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).contains("gone"),
        _ => false,
    }
}

fn has_unique_commits(dir: &Path, branch: &str, base: &str) -> bool {
    let out = Command::new("git")
        .args(["rev-list", "--count"])
        .arg(format!("{}..{}", base, branch))
        .current_dir(dir)
        .output();
    match out {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout).trim().parse::<u32>().unwrap_or(0) > 0
        }
        _ => true, // conservative: don't flag as "no unique" on error
    }
}

fn pr_state(dir: &Path, pr: u32) -> Option<String> {
    let out = Command::new("gh")
        .args([
            "pr",
            "view",
            &pr.to_string(),
            "--json",
            "state",
            "-q",
            ".state",
        ])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}
