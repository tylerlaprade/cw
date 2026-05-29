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

/// Canonicalize, falling back to the input path when it can't be resolved.
fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Workspace number for a concrete dir. The **main worktree is always 0**, even
/// when its directory name happens to match `{stem}_{N}` (e.g. a repo cloned
/// into `app_2` with stem `app`). Without this, `cw remove` would map the main
/// repo to "workspace N" and destroy it. Mirrors the guard in `inventory.rs`.
fn number_for_dir(cfg: &Config, dir: &Path) -> Option<u32> {
    if let Some(root) = cfg.runtime.repo_root.as_deref() {
        let main = worktree::main_worktree(root).unwrap_or_else(|| root.to_path_buf());
        if canonical(dir) == canonical(&main) {
            return Some(0);
        }
    }
    paths::detect_number(dir, &cfg.runtime.stem)
}

fn resolve_cwd(cfg: &Config, cwd: &Path) -> Result<Resolved> {
    let in_workspace = paths::detect_number(cwd, &cfg.runtime.stem).is_some();
    let dir = if in_workspace {
        cwd.to_path_buf()
    } else {
        cfg.runtime
            .repo_root
            .clone()
            .unwrap_or_else(|| cwd.to_path_buf())
    };
    let number = number_for_dir(cfg, &dir);
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
        let pr = branch
            .as_deref()
            .and_then(|b| github::pr_for_branch(&dir, b));
        return Some(Resolved {
            number: Some(0),
            dir,
            branch,
            pr,
        });
    }
    let parent = root.parent()?;
    let dir = parent.join(format!("{}_{}", cfg.runtime.stem, n));
    let dir = if dir.is_dir() {
        dir
    } else {
        // Fall back to the legacy /tmp/{stem}_{n} location (the original's
        // --tmp/swarm placement). cleanup + teardown still recognize these, so
        // the numeric resolver must too — otherwise `cw <N>`/`open`/`restack`
        // can't reach a /tmp workspace that `cw cleanup` will happily list.
        let tmp = Path::new("/tmp").join(format!("{}_{}", cfg.runtime.stem, n));
        if tmp.is_dir() {
            tmp
        } else {
            return None;
        }
    };
    let branch = current_branch(&dir);
    let pr = branch
        .as_deref()
        .and_then(|b| github::pr_for_branch(&dir, b));
    Some(Resolved {
        // 0 when `{stem}_{n}` IS the main worktree, so teardown's workspace-0
        // guard refuses to delete the main repo; otherwise the requested n.
        number: number_for_dir(cfg, &dir),
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
    let pr = github::view_pr(inside, num).with_context(|| format!("resolving PR #{num} via gh"))?;
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
    let number = number_for_dir(cfg, &wt.dir);
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
