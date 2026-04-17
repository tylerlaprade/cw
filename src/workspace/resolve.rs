//! Resolve a user-supplied target (N | PR# | branch | bare cwd) to a
//! concrete workspace directory + number + branch.
//!
//! Step 2 handles the bare case (no target, derive from cwd). PR/branch
//! resolution lands in step 3.

use crate::config::Config;
use crate::util::paths;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// A resolved workspace reference.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub dir: PathBuf,
    pub number: Option<u32>,
    pub branch: Option<String>,
}

/// Resolve `target` against the current config. When `target` is None, the
/// resolver derives from the current cwd.
pub fn resolve(cfg: &Config, cwd: &Path, target: Option<&str>) -> Result<Resolved> {
    let Some(t) = target else {
        return resolve_cwd(cfg, cwd);
    };

    // Numeric target = workspace number.
    if let Ok(n) = t.parse::<u32>() {
        return resolve_number(cfg, n);
    }

    // Otherwise, treat as branch name (PR resolution lands in step 3).
    Err(anyhow::anyhow!(
        "PR / branch target resolution lands in step 3 (got: {t})"
    ))
}

fn resolve_cwd(cfg: &Config, cwd: &Path) -> Result<Resolved> {
    let number = paths::detect_number(cwd, &cfg.runtime.stem);
    let dir = if number.is_some() {
        cwd.to_path_buf()
    } else {
        // Outside a numbered workspace: fall back to the repo root.
        cfg.runtime
            .repo_root
            .clone()
            .unwrap_or_else(|| cwd.to_path_buf())
    };
    let branch = current_branch(&dir);
    Ok(Resolved { dir, number, branch })
}

fn resolve_number(cfg: &Config, n: u32) -> Result<Resolved> {
    let root = cfg
        .runtime
        .repo_root
        .as_deref()
        .context("no repo root discovered")?;
    let parent = root.parent().context("repo root has no parent")?;
    let dir = parent.join(format!("{}_{}", cfg.runtime.stem, n));
    if !dir.is_dir() {
        anyhow::bail!("workspace {} not found at {}", n, dir.display());
    }
    Ok(Resolved {
        number: Some(n),
        branch: current_branch(&dir),
        dir,
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
