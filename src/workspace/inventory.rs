//! Enumerate existing workspaces + their status.

use crate::config::Config;
use crate::git::worktree;
use crate::util::paths;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// Age of the last commit in hours. `None` when git fails (e.g., stale
    /// worktree metadata for a dir that was rm'd).
    pub inactive_hours: Option<u64>,
    /// Age of the worktree *directory* (mtime) in hours. A fresh `cw <desc>`
    /// workspace has no unique commits vs base — indistinguishable from an
    /// abandoned branch by commit metadata alone. The directory mtime is the
    /// only reliable signal that the workspace is new.
    pub dir_age_hours: Option<u64>,
}

impl Entry {
    /// Removable for a *durable* reason: a detached worktree, a branch whose
    /// remote was deleted, or a closed/merged PR. Each requires a prior
    /// push+merge/close (or manual detach), so a freshly-created workspace can
    /// never be in these states — the freshness guard must NOT apply here.
    ///
    /// Note `merged` (`git merge-base --is-ancestor branch base`) is *not*
    /// durable: a brand-new branch off base is trivially an ancestor of base,
    /// so it overlaps with `no_unique_commits` and must be freshness-guarded.
    pub fn is_removable_durable(&self) -> bool {
        self.detached || self.remote_gone || self.pr_closed_or_merged.is_some()
    }

    /// Removable only because it has no unique commits vs base yet (it's
    /// fully-merged / an ancestor of base) — which a brand-new `cw <desc>`
    /// workspace also looks like. The freshness guard applies here (and to
    /// inactivity) to spare just-created workspaces.
    pub fn is_transient_stale(&self, stale_hours: u64) -> bool {
        self.merged || self.no_unique_commits || self.is_inactive(stale_hours)
    }

    /// Treat as a cleanup candidate when idle longer than `stale_hours`.
    /// `stale_hours == 0` DISABLES the inactivity rule (matching the original
    /// cleanup.sh's `[[ $STALE_HOURS -gt 0 ]]` guard) — without this floor a 0
    /// threshold makes `h >= 0` always true and sweeps every workspace.
    pub fn is_inactive(&self, stale_hours: u64) -> bool {
        stale_hours > 0 && self.inactive_hours.is_some_and(|h| h >= stale_hours)
    }

    /// Worktree directory created within `threshold_hours`. Used to spare
    /// freshly-created workspaces from sweeps.
    pub fn is_fresh(&self, threshold_hours: u64) -> bool {
        self.dir_age_hours.is_some_and(|h| h < threshold_hours)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(inactive_hours: Option<u64>, dir_age_hours: Option<u64>) -> Entry {
        Entry {
            number: Some(3),
            dir: PathBuf::from("/tmp/app_3"),
            branch: Some("feat".into()),
            merged: false,
            remote_gone: false,
            detached: false,
            no_unique_commits: false,
            pr_closed_or_merged: None,
            inactive_hours,
            dir_age_hours,
        }
    }

    #[test]
    fn stale_hours_zero_disables_inactivity() {
        // A 0 threshold must DISABLE the inactivity rule (not sweep everything):
        // a long-idle workspace is not inactive, and is_transient_stale is false
        // for it when it has unique commits.
        let e = entry(Some(9999), Some(9999));
        assert!(!e.is_inactive(0), "stale_hours=0 must disable inactivity");
        assert!(
            !e.is_transient_stale(0),
            "a committed, idle workspace is not transient-stale at stale_hours=0"
        );
        // A real threshold still flags it.
        assert!(e.is_inactive(48));
    }
}

pub fn list_workspaces(cfg: &Config) -> Result<Vec<Entry>> {
    let Some(root) = cfg.runtime.repo_root.as_deref() else {
        return Ok(Vec::new());
    };
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let wts = worktree::list(root)?;
    let mut out = Vec::new();
    for w in wts {
        let canonical_dir = std::fs::canonicalize(&w.dir).unwrap_or_else(|_| w.dir.clone());
        let number = if canonical_dir == canonical_root {
            Some(0)
        } else {
            paths::detect_number(&w.dir, &cfg.runtime.stem)
        };
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
            inactive_hours: None,
            dir_age_hours: None,
        };
        e.inactive_hours = last_commit_age_hours(&w.dir);
        e.dir_age_hours = dir_age_hours(&w.dir);
        if let Some(b) = &branch {
            let base = effective_base(root, &cfg.runtime.base_branch);
            e.merged = is_merged(root, b, &base);
            e.remote_gone = remote_gone(root, b);
            e.no_unique_commits = !has_unique_commits(&w.dir, b, &base);
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

fn last_commit_age_hours(dir: &Path) -> Option<u64> {
    let out = Command::new("git")
        .args(["log", "-1", "--format=%ct"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let ts: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(now.saturating_sub(ts) / 3600)
}

fn dir_age_hours(dir: &Path) -> Option<u64> {
    let mtime = std::fs::metadata(dir).ok()?.modified().ok()?;
    let age = SystemTime::now().duration_since(mtime).ok()?;
    Some(age.as_secs() / 3600)
}

fn effective_base(inside: &Path, base: &str) -> String {
    let remote = format!("origin/{base}");
    let exists = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/remotes/{remote}"))
        .current_dir(inside)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if exists {
        remote
    } else {
        base.to_string()
    }
}

fn is_merged(inside: &Path, branch: &str, base: &str) -> bool {
    Command::new("git")
        .args(["merge-base", "--is-ancestor", branch, base])
        .current_dir(inside)
        .output()
        .map(|o| o.status.success())
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
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u32>()
                .unwrap_or(0)
                > 0
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
