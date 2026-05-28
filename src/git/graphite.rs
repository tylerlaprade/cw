//! Stack detection. `find_stack_worktree` finds a worktree whose checked-out
//! branch is in the same stack as `target`, so cw doesn't create a second
//! worktree for the same stack (and restack runs in the right place).
//!
//! Faithful port of `worktree-lib.sh`'s `find_stack_worktree`, generalized:
//! the **git fast path** (commit ancestry) needs no Graphite account and works
//! for any repo; the `gt ls -s` **slow path** is consulted only when `gt` is on
//! PATH, as a fallback when ancestry is stale (mid-rebase).

use crate::git::worktree;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackHit {
    pub dir: PathBuf,
    pub branch: String,
}

/// Find a worktree holding a branch in the same stack as `target` (which must
/// be an existing ref). `base` is the trunk. Returns the first match, or None.
pub fn find_stack_worktree(inside: &Path, target: &str, base: &str) -> Option<StackHit> {
    let worktrees = worktree::list(inside).ok()?;

    // --- Fast path: commit ancestry (no Graphite needed) ---------------------
    // The oldest commit unique to `target` vs `base` is shared by every branch
    // in target's stack.
    let mut stack_branches: Vec<String> = Vec::new();
    if let Some(root) = rev_list_oldest(inside, base, target) {
        stack_branches = branches_containing(inside, &root);

        // Sanity-check against Graphite's recorded parent (when gt is present):
        // if the parent isn't in the commit-based set, ancestry is stale (a
        // partial rebase) — discard so we fall through to the slow path.
        if gt_available() {
            if let Some(parent) = gt_parent(inside, target) {
                if parent != base
                    && parent != "main"
                    && parent != "develop"
                    && !stack_branches.iter().any(|b| *b == parent)
                {
                    stack_branches.clear();
                }
            }
        }
    }

    if !stack_branches.is_empty() {
        stack_branches.sort();
        stack_branches.dedup();
        for sb in &stack_branches {
            if sb == base || sb.is_empty() {
                continue;
            }
            if let Some(w) = worktrees.iter().find(|w| w.branch_name() == Some(sb.as_str())) {
                return Some(StackHit {
                    dir: w.dir.clone(),
                    branch: sb.clone(),
                });
            }
        }
        return None;
    }

    // --- Slow path: `gt ls -s` per worktree (needs Graphite) -----------------
    if !gt_available() {
        return None;
    }
    for w in &worktrees {
        let Some(branch) = w.branch_name() else {
            continue;
        };
        if branch == base {
            continue;
        }
        if gt_stack_contains(&w.dir, target) {
            return Some(StackHit {
                dir: w.dir.clone(),
                branch: branch.to_string(),
            });
        }
    }
    None
}

/// `git rev-list <base>..<target>` → the oldest (last) commit, or None.
fn rev_list_oldest(inside: &Path, base: &str, target: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-list", &format!("{base}..{target}")])
        .current_dir(inside)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .last()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn branches_containing(inside: &Path, sha: &str) -> Vec<String> {
    let out = Command::new("git")
        .args(["branch", "--contains", sha, "--format=%(refname:short)"])
        .current_dir(inside)
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

fn gt_parent(inside: &Path, target: &str) -> Option<String> {
    let out = Command::new("gt")
        .args(["branch", "info", target])
        .current_dir(inside)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("Parent: ").map(|p| p.trim().to_string()))
}

fn gt_stack_contains(dir: &Path, target: &str) -> bool {
    let out = Command::new("gt")
        .args(["ls", "-s", "--no-interactive"])
        .current_dir(dir)
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            // gt ls -s prints one branch per line behind tree-drawing glyphs;
            // a whitespace token equal to target means target is in this stack.
            .lines()
            .any(|line| line.split_whitespace().any(|tok| tok == target)),
        _ => false,
    }
}

fn gt_available() -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join("gt").is_file()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?} failed");
    }

    fn commit(dir: &Path, file: &str) {
        std::fs::write(dir.join(file), file).unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", file, "--quiet"]);
    }

    // Git fast path (no Graphite): branch B is stacked on A (both off develop);
    // A is checked out in a sibling worktree. Restacking/creating B should find
    // A's worktree as the shared-stack worktree.
    #[test]
    fn fast_path_finds_stack_sibling_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--initial-branch=develop"]);
        git(&repo, &["config", "user.email", "t@t.local"]);
        git(&repo, &["config", "user.name", "T"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        commit(&repo, "root.txt");
        git(&repo, &["checkout", "-b", "feat-a"]);
        commit(&repo, "a.txt");
        git(&repo, &["checkout", "-b", "feat-b"]);
        commit(&repo, "b.txt");
        git(&repo, &["checkout", "develop"]);

        let wt_a = tmp.path().join("repo_1");
        git(&repo, &["worktree", "add", wt_a.to_str().unwrap(), "feat-a"]);

        let hit = find_stack_worktree(&repo, "feat-b", "develop").expect("stack sibling found");
        assert_eq!(hit.branch, "feat-a");
        assert_eq!(
            std::fs::canonicalize(&hit.dir).unwrap(),
            std::fs::canonicalize(&wt_a).unwrap()
        );
    }

    #[test]
    fn no_match_when_no_sibling_checked_out() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--initial-branch=develop"]);
        git(&repo, &["config", "user.email", "t@t.local"]);
        git(&repo, &["config", "user.name", "T"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        commit(&repo, "root.txt");
        git(&repo, &["checkout", "-b", "feat-a"]);
        commit(&repo, "a.txt");
        git(&repo, &["checkout", "develop"]);
        // feat-a exists but is not in any worktree → no hit.
        assert_eq!(find_stack_worktree(&repo, "feat-a", "develop"), None);
    }
}
