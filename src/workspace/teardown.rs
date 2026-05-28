//! Workspace teardown: safety-check, drop DBs, prune worktree, close tab.

use crate::config::{schema::DatabasesCfg, Config};
use crate::shell::{Emitter, Record};
use crate::util::paths;
use crate::workspace::resolve;
use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct RemoveOpts {
    pub force: bool,
    pub dry_run: bool,
    pub no_close_tab: bool,
    /// Hours of inactivity after which a workspace with an open/draft PR is
    /// still eligible for removal (when there's no active shell session).
    /// `None` disables the override (strict: open/draft PRs always block).
    pub stale_hours: Option<u64>,
}

impl Default for RemoveOpts {
    fn default() -> Self {
        Self {
            force: false,
            dry_run: false,
            no_close_tab: false,
            stale_hours: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub number: u32,
    pub dir: PathBuf,
    pub branch: Option<String>,
    pub pr: Option<u32>,
    pub pr_state: Option<PrState>,
    pub uncommitted: bool,
    pub unique_commits: u32,
    pub active_session: bool,
    pub inactive_hours: Option<u64>,
    pub databases: Vec<String>,
    pub head_short: Option<String>,
    pub is_cwd: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Draft,
    Merged,
    Closed,
}

impl PrState {
    fn label(self) -> &'static str {
        match self {
            PrState::Open => "open",
            PrState::Draft => "draft",
            PrState::Merged => "merged",
            PrState::Closed => "closed",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Verdict {
    Clean(String),
    Dirty(String),
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
        if n == 0 {
            anyhow::bail!("refuse to remove workspace 0 (repo root)");
        }
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

    // Compute verdicts and print legacy-style report.
    let verdicts: Vec<Verdict> = plans.iter().map(|p| verdict(p, opts)).collect();
    for (p, v) in plans.iter().zip(verdicts.iter()) {
        let branch_disp = p.branch.as_deref().unwrap_or("HEAD");
        match v {
            Verdict::Clean(reason) => {
                println!(
                    "  [{}] {}  {} ({}) — {}",
                    p.number,
                    "CLEAN".green(),
                    p.dir.display(),
                    branch_disp,
                    reason
                );
            }
            Verdict::Dirty(reason) => {
                println!(
                    "  [{}] {}  {} ({}) — {}",
                    p.number,
                    "DIRTY".red(),
                    p.dir.display(),
                    branch_disp,
                    reason
                );
                if let Some(h) = &p.head_short {
                    println!("         {}", h);
                }
                println!("  [{}] Skipping (use --force to override)", p.number);
            }
        }
    }

    if opts.dry_run {
        println!("{} --dry-run; not removing", "·".dimmed());
        return Ok(());
    }

    let mut closed_tab_target = false;
    for (p, v) in plans.iter().zip(verdicts.iter()) {
        let is_dirty = matches!(v, Verdict::Dirty(_));
        if !opts.force && is_dirty {
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
        pr_state: None,
        uncommitted: false,
        unique_commits: 0,
        active_session: false,
        inactive_hours: None,
        databases,
        head_short: None,
        is_cwd,
    })
}

fn safety_check(cfg: &Config, p: &mut Plan) {
    if !p.dir.is_dir() {
        return;
    }
    p.uncommitted = is_dirty(&p.dir);
    p.head_short = head_short(&p.dir);
    p.inactive_hours = last_commit_age_hours(&p.dir);
    let Some(branch) = &p.branch else {
        return;
    };
    p.unique_commits = commits_ahead(&p.dir, &cfg.runtime.base_branch, branch).unwrap_or(0);
    if p.unique_commits == 0 {
        p.active_session = has_active_session(&p.dir);
    }
    if let Some(pr) = p.pr {
        p.pr_state = pr_state(&p.dir, pr);
    }
}

fn verdict(p: &Plan, opts: &RemoveOpts) -> Verdict {
    if p.uncommitted {
        return Verdict::Dirty("uncommitted changes".into());
    }
    // No unique commits vs base: clean unless a shell sits inside it.
    if p.branch.is_some() && p.unique_commits == 0 {
        if p.active_session {
            return Verdict::Dirty("active session".into());
        }
        return Verdict::Clean("no unique work, no active session".into());
    }
    // Consult PR state.
    if let (Some(pr), Some(state)) = (p.pr, p.pr_state) {
        match state {
            PrState::Merged | PrState::Closed => {
                return Verdict::Clean(format!("PR {}", state.label()));
            }
            PrState::Open | PrState::Draft => {
                if let Some(stale) = opts.stale_hours {
                    if let Some(h) = p.inactive_hours {
                        if h >= stale && !p.active_session {
                            return Verdict::Clean(format!(
                                "PR {}, inactive {}h",
                                state.label(),
                                h
                            ));
                        }
                    }
                }
                let _ = pr;
                return Verdict::Dirty(format!("PR {}", state.label()));
            }
        }
    }
    // Has unique commits and no PR resolution.
    if p.branch.is_none() {
        return Verdict::Dirty("detached / no branch".into());
    }
    Verdict::Dirty("no PR found".into())
}

fn is_dirty(dir: &Path) -> bool {
    Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir)
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

fn head_short(dir: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["log", "-1", "--format=%h %s"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
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

fn pr_state(dir: &Path, pr: u32) -> Option<PrState> {
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
    let state = fields.first().copied().unwrap_or("");
    if fields.get(1).is_some_and(|f| *f == "DRAFT") {
        return Some(PrState::Draft);
    }
    match state {
        "OPEN" | "open" => Some(PrState::Open),
        "MERGED" | "merged" => Some(PrState::Merged),
        "CLOSED" | "closed" => Some(PrState::Closed),
        _ => None,
    }
}

/// Return true if a non-ancestor process has its cwd rooted in `dir`.
/// Uses `lsof -d cwd` and excludes our own process-tree ancestors so the
/// script invoking the check doesn't flag itself.
fn has_active_session(dir: &Path) -> bool {
    let mut ancestors: Vec<u32> = Vec::new();
    let mut pid = std::process::id();
    while pid > 1 {
        ancestors.push(pid);
        pid = parent_pid(pid).unwrap_or(0);
        if pid == 0 {
            break;
        }
    }
    let out = Command::new("lsof")
        .args([
            "-d", "cwd", "-c", "zsh", "-c", "bash", "-c", "sh", "-c", "fish", "-c", "node",
        ])
        .output();
    let Ok(out) = out else {
        return false;
    };
    let dir_s = dir.to_string_lossy();
    let dir_prefix = format!("{}/", dir_s);
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 2 {
            continue;
        }
        let Ok(lpid) = fields[1].parse::<u32>() else {
            continue;
        };
        // `NAME` (cwd path) is the last field.
        let Some(last) = fields.last() else {
            continue;
        };
        if *last != dir_s.as_ref() && !last.starts_with(&dir_prefix) {
            continue;
        }
        if !ancestors.contains(&lpid) {
            return true;
        }
    }
    false
}

fn parent_pid(pid: u32) -> Option<u32> {
    let out = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
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
    cmd.arg("-c")
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
        .output()?;
    if !st.status.success() {
        // Fallback: remove the dir tree + prune.
        let _ = std::fs::remove_dir_all(&p.dir);
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(run_in)
            .output();
    }

    // Delete the branch too (ignore failures).
    if let Some(branch) = &p.branch {
        let _ = Command::new("git")
            .args(["branch", "-D", branch])
            .current_dir(run_in)
            .output();
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
