//! `cw cleanup`: sweep stale workspaces + branches + orphaned DBs.

use crate::cli::CleanupArgs;
use crate::config::{self, Config};
use crate::shell::Emitter;
use crate::workspace::{inventory, teardown};
use anyhow::Result;
use owo_colors::OwoColorize;
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

/// Hours of inactivity before a workspace with an open/draft PR is still
/// eligible for cleanup. Mirrors `STALE_HOURS=48` in the legacy cleanup.sh.
const STALE_HOURS: u64 = 48;

pub fn run(args: CleanupArgs, emitter: &mut Emitter) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let cfg = config::discover::load(&cwd)?;
    let Some(root) = cfg.runtime.repo_root.clone() else {
        anyhow::bail!("not inside a git repo");
    };

    // I4: fetch+prune even on --dry-run so the stale/gone-branch preview is
    // accurate (the original cleanup.sh always fetched). It only updates remote-
    // tracking refs — no local work is touched.
    println!("{} git fetch --prune origin", "→".cyan());
    let _ = Command::new("git")
        .args(["fetch", "--prune", "origin"])
        .current_dir(&root)
        .status();

    let entries = inventory::list_workspaces(&cfg)?;
    let stale: Vec<_> = entries
        .iter()
        .filter(|e| {
            e.dir != root
                && e.number.is_some()
                && (e.is_removable() || e.is_inactive(STALE_HOURS))
                // A freshly-created workspace has no unique commits vs base
                // and looks identical to an abandoned branch. Spare anything
                // whose directory was created within STALE_HOURS.
                && !e.is_fresh(STALE_HOURS)
        })
        .cloned()
        .collect();

    if stale.is_empty() {
        println!("{} no stale workspaces", "✓".green());
    } else {
        for e in &stale {
            print_candidate(e);
        }
        println!();
    }

    if !args.dry_run && !stale.is_empty() {
        let targets: Vec<String> = stale
            .iter()
            .map(|e| e.number.unwrap().to_string())
            .collect();
        teardown::run(
            &cfg,
            &targets,
            &teardown::RemoveOpts {
                force: args.force,
                dry_run: false,
                no_close_tab: false,
                stale_hours: Some(STALE_HOURS),
            },
            emitter,
        )?;
    }

    // Prune local branches (merged / remote-gone / closed-PR) that aren't
    // checked out in any worktree.
    if !args.dry_run {
        prune_branches(&root, &cfg.runtime.base_branch);
        delete_closed_pr_branches(&root, &cfg.runtime.base_branch);
    }

    // Orphaned DBs (pattern exists but no {stem}_{N} dir).
    if let Some(db) = &cfg.databases {
        warn_orphaned_dbs(&cfg, db);
    }

    // Graphite sync at the end.
    if graphite_enabled(&cfg) && !args.dry_run {
        println!("{} gt sync", "→".cyan());
        let mut cmd = Command::new("gt");
        cmd.arg("sync").current_dir(&root);
        if args.force {
            cmd.arg("--force");
        }
        let _ = cmd.status();
    }
    Ok(())
}

fn print_candidate(e: &inventory::Entry) {
    let branch_disp = e.branch.as_deref().unwrap_or("HEAD");
    if e.detached {
        println!("Detached worktree: {}", e.dir.display());
    } else if e.remote_gone {
        println!("Merged branch: {} ({})", e.dir.display(), branch_disp);
    } else if e.merged || e.no_unique_commits {
        println!("No unique commits: {} ({})", e.dir.display(), branch_disp);
    } else if let Some(h) = e.inactive_hours.filter(|h| *h >= STALE_HOURS) {
        println!("Inactive {}h: {} ({})", h, e.dir.display(), branch_disp);
    } else if e.pr_closed_or_merged.is_some() {
        println!("Merged branch: {} ({})", e.dir.display(), branch_disp);
    }
}

/// Delete local branches fully merged into base OR whose remote was pruned,
/// excluding protected branches and any branch currently checked out in a
/// worktree.
fn prune_branches(root: &Path, base: &str) {
    let protected = protected_branches(base);

    let checked_out = worktree_branches(root);
    let merged_ref = remote_or_local_base(root, base);
    let merged = list_branches(
        root,
        &["branch", "--merged", &merged_ref, "--format=%(refname:short)"],
    );
    let gone = gone_upstream_branches(root);

    let mut to_delete: Vec<String> = Vec::new();
    for b in merged.iter().chain(gone.iter()) {
        if b.is_empty() || protected.contains(b.as_str()) {
            continue;
        }
        if checked_out.contains(b) {
            continue;
        }
        if !to_delete.contains(b) {
            to_delete.push(b.clone());
        }
    }
    if to_delete.is_empty() {
        return;
    }
    println!("{} deleting {} stale branch(es)", "·".dimmed(), to_delete.len());
    for b in &to_delete {
        let _ = Command::new("git")
            .args(["branch", "-D", b])
            .current_dir(root)
            .output();
    }
}

/// Delete local branches whose PR is closed (not merged — merged PRs are
/// typically already handled via gone-upstream pruning).
fn delete_closed_pr_branches(root: &Path, base: &str) {
    let out = Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "closed",
            "--json",
            "headRefName,state",
            "--limit",
            "500",
        ])
        .current_dir(root)
        .output();
    let Ok(out) = out else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // Minimal JSON scan: find "headRefName":"..." pairs next to "state":"CLOSED".
    // The schema is `[{"headRefName":"...", "state":"CLOSED|MERGED"}, ...]`.
    let mut closed_branches: Vec<String> = Vec::new();
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        if let Some(k) = find_key(&text, i, "headRefName") {
            if let Some((name, next)) = read_string_after(&text, k) {
                let state_start = next;
                if let Some(sk) = find_key(&text, state_start, "state") {
                    if let Some((state, after)) = read_string_after(&text, sk) {
                        if state == "CLOSED" {
                            closed_branches.push(name);
                        }
                        i = after;
                        continue;
                    }
                }
                i = next;
                continue;
            }
        }
        break;
    }

    if closed_branches.is_empty() {
        return;
    }
    let checked_out = worktree_branches(root);
    let protected = protected_branches(base);
    for b in &closed_branches {
        if protected.contains(b.as_str()) || checked_out.contains(b) {
            continue;
        }
        // Skip if the branch doesn't exist locally.
        let ok = Command::new("git")
            .args(["rev-parse", "--verify", b])
            .current_dir(root)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            continue;
        }
        println!("{} deleting closed-PR branch: {}", "·".dimmed(), b);
        let _ = Command::new("git")
            .args(["branch", "-D", b])
            .current_dir(root)
            .output();
    }
}

/// Branches never pruned: the configured base plus the conventional trunk
/// names (so we don't delete a trunk even if it isn't the configured base).
fn protected_branches(base: &str) -> HashSet<String> {
    let mut p: HashSet<String> = ["main", "master", "develop"]
        .into_iter()
        .map(String::from)
        .collect();
    p.insert(base.to_string());
    p
}

/// `origin/<base>` when that remote-tracking ref exists, else the local base —
/// so "merged into trunk" is judged against the remote like the original.
fn remote_or_local_base(root: &Path, base: &str) -> String {
    let remote = format!("origin/{base}");
    let exists = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/remotes/{remote}"))
        .current_dir(root)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if exists {
        remote
    } else {
        base.to_string()
    }
}

fn worktree_branches(root: &Path) -> HashSet<String> {
    let mut set = HashSet::new();
    let Ok(out) = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(root)
        .output()
    else {
        return set;
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(b) = line.strip_prefix("branch refs/heads/") {
            set.insert(b.to_string());
        }
    }
    set
}

fn list_branches(root: &Path, args: &[&str]) -> Vec<String> {
    let Ok(out) = Command::new("git").args(args).current_dir(root).output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().trim_start_matches('*').trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

fn gone_upstream_branches(root: &Path) -> Vec<String> {
    let Ok(out) = Command::new("git")
        .args([
            "for-each-ref",
            "--format=%(refname:short) %(upstream:track)",
            "refs/heads/",
        ])
        .current_dir(root)
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let mut v = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if line.contains("[gone]") {
            if let Some(name) = line.split_whitespace().next() {
                v.push(name.to_string());
            }
        }
    }
    v
}

fn find_key(haystack: &str, from: usize, key: &str) -> Option<usize> {
    let quoted = format!("\"{}\"", key);
    haystack[from..].find(&quoted).map(|i| from + i + quoted.len())
}

/// Find the next JSON string after the given byte offset and return
/// (content, byte-offset after the closing quote).
fn read_string_after(haystack: &str, from: usize) -> Option<(String, usize)> {
    let s = &haystack[from..];
    let open = s.find('"')?;
    let rest = &s[open + 1..];
    let mut out = String::new();
    let mut chars = rest.char_indices();
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            if let Some((_, esc)) = chars.next() {
                out.push(esc);
            }
            continue;
        }
        if c == '"' {
            return Some((out, from + open + 1 + i + 1));
        }
        out.push(c);
    }
    None
}

fn warn_orphaned_dbs(cfg: &Config, db: &crate::config::schema::DatabasesCfg) {
    // I3: without {n} the pattern can't map a DB back to a workspace number, and
    // the regex would have no capture group 1 (the old `caps.get(1).unwrap()`
    // panicked). Nothing to cross-reference — skip.
    if !db.pattern.contains("{n}") {
        return;
    }
    let stem = cfg.runtime.stem.clone();
    let suffix_group = db.suffixes.join("|");
    let pat = db
        .pattern
        .replace("{n}", r"(\d+)")
        .replace("{suffix}", &format!("({})", suffix_group));
    let Ok(re) = regex::Regex::new(&format!("^{}$", pat)) else {
        return;
    };

    let out = Command::new("psql")
        .args(["-At", "-c", "SELECT datname FROM pg_database"])
        .output();
    let Ok(out) = out else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let Some(parent) = cfg.runtime.repo_root.as_deref().and_then(|r| r.parent()) else {
        return;
    };
    let mut warned = 0;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some(caps) = re.captures(line) else {
            continue;
        };
        let Some(n) = caps.get(1).and_then(|m| m.as_str().parse::<u32>().ok()) else {
            continue;
        };
        let dir = parent.join(format!("{}_{}", stem, n));
        if !dir.is_dir() {
            println!(
                "{} orphaned DB: {} (no matching workspace dir)",
                "⚠".yellow(),
                line
            );
            warned += 1;
        }
    }
    if warned > 0 {
        println!("  (drop manually or pass --force on `cw remove <N>` after recreating the dir)");
    }
}

fn graphite_enabled(cfg: &Config) -> bool {
    cfg.integrations.graphite.unwrap_or_else(|| {
        std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).any(|d| d.join("gt").is_file()))
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::protected_branches;

    #[test]
    fn protected_includes_base_and_trunks() {
        // I1: a non-develop base (e.g. main) is protected, and the conventional
        // trunks stay protected too — no hardcoded company-specific release branches.
        let p = protected_branches("main");
        assert!(p.contains("main"));
        assert!(p.contains("master"));
        assert!(p.contains("develop"));
        // A custom base is protected.
        assert!(protected_branches("trunk").contains("trunk"));
    }
}
