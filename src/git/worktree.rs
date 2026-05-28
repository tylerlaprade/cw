//! `git worktree` helpers.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct Worktree {
    pub dir: PathBuf,
    pub head: Option<String>,
    /// Full ref (e.g. `refs/heads/my-branch`). None when detached.
    pub branch_ref: Option<String>,
}

impl Worktree {
    pub fn branch_name(&self) -> Option<&str> {
        self.branch_ref
            .as_deref()
            .and_then(|r| r.strip_prefix("refs/heads/"))
    }
}

/// Parse `git worktree list --porcelain` from any dir inside the repo.
pub fn list(inside: &Path) -> Result<Vec<Worktree>> {
    let out = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(inside)
        .output()
        .context("running `git worktree list --porcelain`")?;
    if !out.status.success() {
        anyhow::bail!(
            "git worktree list failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(parse_porcelain(&String::from_utf8_lossy(&out.stdout)))
}

/// Find the worktree whose checked-out branch equals `branch`.
pub fn find_for_branch(inside: &Path, branch: &str) -> Result<Option<Worktree>> {
    Ok(list(inside)?
        .into_iter()
        .find(|w| w.branch_name() == Some(branch)))
}

fn parse_porcelain(s: &str) -> Vec<Worktree> {
    let mut out = Vec::new();
    let mut cur: Option<Worktree> = None;
    for line in s.lines() {
        if line.is_empty() {
            if let Some(w) = cur.take() {
                out.push(w);
            }
            continue;
        }
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some(w) = cur.take() {
                out.push(w);
            }
            cur = Some(Worktree {
                dir: PathBuf::from(p),
                head: None,
                branch_ref: None,
            });
        } else if let Some(h) = line.strip_prefix("HEAD ") {
            if let Some(w) = cur.as_mut() {
                w.head = Some(h.to_string());
            }
        } else if let Some(b) = line.strip_prefix("branch ") {
            if let Some(w) = cur.as_mut() {
                w.branch_ref = Some(b.to_string());
            }
        }
    }
    if let Some(w) = cur.take() {
        out.push(w);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_porcelain() {
        let input = "\
worktree /main
HEAD abc123
branch refs/heads/develop

worktree /side
HEAD def456
branch refs/heads/feature/x

worktree /detached
HEAD fff888
detached
";
        let ws = parse_porcelain(input);
        assert_eq!(ws.len(), 3);
        assert_eq!(ws[0].branch_name(), Some("develop"));
        assert_eq!(ws[1].branch_name(), Some("feature/x"));
        assert_eq!(ws[2].branch_name(), None);
    }
}
