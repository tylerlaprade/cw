//! Workspace teardown: safety-check, drop DBs, prune worktree, close tab.

use crate::config::{schema::DatabasesCfg, Config};
use crate::shell::{Emitter, Record};
use crate::util::paths;
use crate::workspace::resolve;
use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct RemoveOpts {
    pub force: bool,
    pub dry_run: bool,
    pub no_close_tab: bool,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub number: u32,
    pub dir: PathBuf,
    pub branch: Option<String>,
    pub pr: Option<u32>,
    pub status: Vec<Flag>,
    pub databases: Vec<String>,
    pub is_cwd: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Flag {
    Uncommitted,
    UniqueCommits(u32),
    PrOpen(u32),
    PrDraft(u32),
    PrMerged(u32),
    PrClosed(u32),
    NoBranch,
}

impl Flag {
    fn is_blocking(&self) -> bool {
        matches!(
            self,
            Flag::Uncommitted | Flag::UniqueCommits(_) | Flag::PrOpen(_) | Flag::PrDraft(_)
        )
    }
    fn label(&self) -> String {
        match self {
            Flag::Uncommitted => "uncommitted changes".into(),
            Flag::UniqueCommits(n) => format!("{n} unpushed commit(s) vs base"),
            Flag::PrOpen(n) => format!("PR #{n} open"),
            Flag::PrDraft(n) => format!("PR #{n} draft"),
            Flag::PrMerged(n) => format!("PR #{n} merged"),
            Flag::PrClosed(n) => format!("PR #{n} closed"),
            Flag::NoBranch => "detached / no branch".into(),
        }
    }
}

pub fn run(cfg: &Config, targets: &[String], opts: &RemoveOpts, emitter: &mut Emitter) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let targets = if targets.is_empty() {
        match paths::detect_number(&cwd, &cfg.runtime.stem) {
            Some(n) => vec![n.to_string()],
            None => anyhow::bail!(
                "`cw remove` requires one or more targets unless run from inside a numbered workspace"
            ),
        }
    } else {
        targets.to_vec()
    };
    let mut plans = Vec::new();
    for t in &targets {
        let r = resolve::resolve(cfg, &cwd, Some(t))?;
        let Some(n) = r.number else {
            eprintln!(
                "{} {} does not resolve to a numbered workspace — skipping",
                "✗".red(),
                t
            );
            continue;
        };
        let databases = database_names_for(cfg, n);
        let plan = build_plan(cfg, n, r, databases, &cwd)?;
        plans.push(plan);
    }
    if plans.is_empty() {
        return Ok(());
    }

    // Run safety checks in parallel.
    std::thread::scope(|s| {
        let handles: Vec<_> = plans
            .iter_mut()
            .map(|p| s.spawn(|| safety_check(cfg, p)))
            .collect();
        for h in handles {
            let _ = h.join();
        }
    });

    // Report.
    for p in &plans {
        let flags = if p.status.is_empty() {
            "clean".green().to_string()
        } else {
            p.status
                .iter()
                .map(|f| {
                    if f.is_blocking() {
                        f.label().red().to_string()
                    } else {
                        f.label().yellow().to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!(
            "{} #{:<3} {} — {}",
            "·".dimmed(),
            p.number,
            p.dir.display(),
            flags
        );
    }

    if opts.dry_run {
        println!("{} --dry-run; not removing", "·".dimmed());
        return Ok(());
    }

    let mut closed_tab_target = false;
    for p in &plans {
        if !opts.force && p.status.iter().any(Flag::is_blocking) {
            eprintln!(
                "{} #{} skipped (blocking flags; pass --force to override)",
                "✗".red(),
                p.number
            );
            continue;
        }

        if let Err(e) = run_pre_remove_hook(cfg, p) {
            eprintln!(
                "{} #{} pre-remove hook failed: {:#}",
                "✗".red(),
                p.number,
                e
            );
            continue;
        }

        // Drop DBs in parallel.
        drop_databases(&p.databases);

        // Remove worktree. Critical: if we're in cwd, git worktree commands
        // would fail after the dir is gone — cd to / first.
        if let Err(e) = remove_workspace_dir(cfg, p) {
            eprintln!(
                "{} #{} removal failed: {:#}",
                "✗".red(),
                p.number,
                e
            );
            continue;
        }
        println!("{} #{} removed", "✓".green(), p.number);

        if p.is_cwd && !opts.no_close_tab {
            closed_tab_target = true;
        }
    }

    if closed_tab_target {
        emitter.emit(Record::CloseTab);
    }
    Ok(())
}

fn database_names_for(cfg: &Config, n: u32) -> Vec<String> {
    let Some(db) = &cfg.databases else {
        return Vec::new();
    };
    expand_db_names(db, n)
}

pub fn expand_db_names(db: &DatabasesCfg, n: u32) -> Vec<String> {
    db.suffixes
        .iter()
        .map(|s| {
            db.pattern
                .replace("{n}", &n.to_string())
                .replace("{suffix}", s)
        })
        .collect()
}

fn build_plan(
    _cfg: &Config,
    n: u32,
    r: resolve::Resolved,
    databases: Vec<String>,
    cwd: &Path,
) -> Result<Plan> {
    let is_cwd = cwd.starts_with(&r.dir);
    Ok(Plan {
        number: n,
        dir: r.dir,
        branch: r.branch,
        pr: r.pr,
        status: Vec::new(),
        databases,
        is_cwd,
    })
}

fn safety_check(cfg: &Config, p: &mut Plan) {
    if !p.dir.is_dir() {
        return;
    }
    if is_dirty(&p.dir) {
        p.status.push(Flag::Uncommitted);
    }
    let Some(branch) = &p.branch else {
        p.status.push(Flag::NoBranch);
        return;
    };
    if let Some(n) = commits_ahead(&p.dir, &cfg.runtime.base_branch, branch) {
        if n > 0 {
            p.status.push(Flag::UniqueCommits(n));
        }
    }
    if let Some(pr) = p.pr {
        if let Some(state) = pr_state(&p.dir, pr) {
            match state.as_str() {
                "OPEN" | "open" => p.status.push(Flag::PrOpen(pr)),
                "MERGED" | "merged" => p.status.push(Flag::PrMerged(pr)),
                "CLOSED" | "closed" => p.status.push(Flag::PrClosed(pr)),
                _ => {}
            }
        }
    }
}

fn is_dirty(dir: &Path) -> bool {
    Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir)
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

fn commits_ahead(dir: &Path, base: &str, branch: &str) -> Option<u32> {
    let out = Command::new("git")
        .args(["rev-list", "--count"])
        .arg(format!("{}..{}", base, branch))
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

fn pr_state(dir: &Path, pr: u32) -> Option<String> {
    let out = Command::new("gh")
        .args([
            "pr",
            "view",
            &pr.to_string(),
            "--json",
            "state,isDraft",
            "-q",
            "[.state, (if .isDraft then \"DRAFT\" else \"\" end)] | @tsv",
        ])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    let fields: Vec<&str> = s.split('\t').collect();
    let state = fields.first().copied().unwrap_or("").to_string();
    if fields.get(1).is_some_and(|f| *f == "DRAFT") {
        return Some("DRAFT".into());
    }
    Some(state)
}

fn drop_databases(names: &[String]) {
    std::thread::scope(|s| {
        let handles: Vec<_> = names
            .iter()
            .map(|name| s.spawn(move || drop_one(name)))
            .collect();
        for h in handles {
            let _ = h.join();
        }
    });
}

fn drop_one(name: &str) {
    let st = Command::new("dropdb")
        .args(["--if-exists", name])
        .status();
    match st {
        Ok(s) if s.success() => println!("{} dropdb {}", "·".dimmed(), name),
        _ => {}
    }
}

fn run_pre_remove_hook(cfg: &Config, p: &Plan) -> Result<()> {
    let Some(hook) = &cfg.hooks.pre_remove else {
        return Ok(());
    };
    let current_dir = if p.dir.is_dir() {
        p.dir.as_path()
    } else {
        cfg.runtime
            .repo_root
            .as_deref()
            .unwrap_or_else(|| p.dir.as_path())
    };
    let mut cmd = Command::new("bash");
    cmd.arg("-lc")
        .arg(hook)
        .current_dir(current_dir)
        .env("DEVCLI_DIR", &p.dir)
        .env("DEVCLI_NUMBER", p.number.to_string());
    if let Some(branch) = &p.branch {
        cmd.env("DEVCLI_BRANCH", branch);
    }
    let status = cmd
        .status()
        .with_context(|| format!("running pre-remove hook in {}", current_dir.display()))?;
    if !status.success() {
        anyhow::bail!("hook exited with status {}", status.code().unwrap_or(-1));
    }
    Ok(())
}

fn remove_workspace_dir(_cfg: &Config, p: &Plan) -> Result<()> {
    // Kill any processes rooted in the dir first (services, tail -f, etc).
    let _ = Command::new("pkill")
        .args(["-f", &p.dir.to_string_lossy()])
        .status();

    // If cwd is inside the target, step out before running git commands.
    if p.is_cwd {
        std::env::set_current_dir("/")?;
    }

    // Purge heavy untracked dirs so `git worktree remove` doesn't choke.
    for heavy in ["node_modules", ".venv", "dist", "build"] {
        let _ = std::fs::remove_dir_all(p.dir.join(heavy));
    }

    // git worktree remove --force. Run from the repo's main worktree if we
    // can find one, since running inside the doomed worktree is problematic.
    let git_root = main_worktree_near(&p.dir);
    let run_in = git_root.as_deref().unwrap_or_else(|| Path::new("/"));

    let st = Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&p.dir)
        .current_dir(run_in)
        .status()?;
    if !st.success() {
        // Fallback: remove the dir tree + prune.
        let _ = std::fs::remove_dir_all(&p.dir);
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(run_in)
            .status();
    }

    // Delete the branch too (ignore failures).
    if let Some(branch) = &p.branch {
        let _ = Command::new("git")
            .args(["branch", "-D", branch])
            .current_dir(run_in)
            .status();
    }

    Ok(())
}

fn main_worktree_near(from: &Path) -> Option<PathBuf> {
    // `git worktree list --porcelain` starts with the main worktree.
    let out = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(from.parent().unwrap_or(from))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            return Some(PathBuf::from(p));
        }
    }
    None
}
