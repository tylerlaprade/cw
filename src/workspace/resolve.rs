//! Resolve a user-supplied target (N | PR# | branch | bare cwd) to a
//! concrete workspace directory + number + branch.
//!
//! Heuristic: any numeric token ≤ max_count (or ≤ 99 when unset) is treated
//! as a workspace number; otherwise as a PR number (and, if local matching
//! fails, as a branch name).

use crate::config::Config;
use crate::git::{github, worktree};
use crate::util::paths;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Resolved {
    pub dir: PathBuf,
    pub number: Option<u32>,
    pub branch: Option<String>,
    pub pr: Option<u32>,
}

pub fn resolve(cfg: &Config, cwd: &Path, target: Option<&str>) -> Result<Resolved> {
    let Some(t) = target else {
        return resolve_cwd(cfg, cwd);
    };

    if let Ok(n) = t.parse::<u32>() {
        let cap = cfg.workspace.max_count.unwrap_or(99);
        if n <= cap {
            if let Some(r) = try_number(cfg, n) {
                return Ok(r);
            }
            // Numeric but no workspace exists at that number → fall through
            // and try as PR.
        }
        return resolve_pr(cfg, n);
    }

    resolve_branch(cfg, t)
}

fn resolve_cwd(cfg: &Config, cwd: &Path) -> Result<Resolved> {
    let number = paths::detect_number(cwd, &cfg.runtime.stem);
    let dir = if number.is_some() {
        cwd.to_path_buf()
    } else {
        cfg.runtime
            .repo_root
            .clone()
            .unwrap_or_else(|| cwd.to_path_buf())
    };
    let branch = current_branch(&dir);
    let pr = branch
        .as_deref()
        .and_then(|b| github::pr_for_branch(&dir, b));
    Ok(Resolved {
        dir,
        number,
        branch,
        pr,
    })
}

fn try_number(cfg: &Config, n: u32) -> Option<Resolved> {
    let root = cfg.runtime.repo_root.as_deref()?;
    if n == 0 {
        let dir = worktree::main_worktree(root).unwrap_or_else(|| root.to_path_buf());
        let branch = current_branch(&dir);
        let pr = branch.as_deref().and_then(|b| github::pr_for_branch(&dir, b));
        return Some(Resolved {
            number: Some(0),
            dir,
            branch,
            pr,
        });
    }
    let parent = root.parent()?;
    let dir = parent.join(format!("{}_{}", cfg.runtime.stem, n));
    if !dir.is_dir() {
        return None;
    }
    let branch = current_branch(&dir);
    let pr = branch.as_deref().and_then(|b| github::pr_for_branch(&dir, b));
    Some(Resolved {
        number: Some(n),
        dir,
        branch,
        pr,
    })
}

fn resolve_pr(cfg: &Config, num: u32) -> Result<Resolved> {
    let inside = cfg
        .runtime
        .repo_root
        .as_deref()
        .context("no repo root discovered")?;
    let pr = github::view_pr(inside, num)
        .with_context(|| format!("resolving PR #{num} via gh"))?;
    let mut r = resolve_branch(cfg, &pr.head_branch)?;
    r.pr = Some(num);
    Ok(r)
}

fn resolve_branch(cfg: &Config, branch: &str) -> Result<Resolved> {
    let inside = cfg
        .runtime
        .repo_root
        .as_deref()
        .context("no repo root discovered")?;
    let wt = worktree::find_for_branch(inside, branch)?
        .with_context(|| format!("no worktree checking out branch {branch}"))?;
    let number = paths::detect_number(&wt.dir, &cfg.runtime.stem);
    let pr = github::pr_for_branch(&wt.dir, branch);
    Ok(Resolved {
        dir: wt.dir,
        number,
        branch: Some(branch.to_string()),
        pr,
    })
}

fn current_branch(dir: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() || s == "HEAD" {
        None
    } else {
        Some(s)
    }
}
