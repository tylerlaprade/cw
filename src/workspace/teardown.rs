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

#[derive(Debug, Clone, Default)]
pub struct RemoveOpts {
    pub force: bool,
    pub dry_run: bool,
    pub no_close_tab: bool,
    /// Hours of inactivity after which a workspace with an open/draft PR is
    /// still eligible for removal (when there's no active shell session).
    /// `None` disables the override (strict: open/draft PRs always block).
    pub stale_hours: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub number: u32,
    pub dir: PathBuf,
    pub branch: Option<String>,
    pub pr: Option<u32>,
    pub pr_state: Option<PrState>,
    pub uncommitted: bool,
    /// Commits on the branch not contained in the (remote) base branch.
    /// `None` means git could not determine it — treated as "has work" so a
    /// failed lookup never reads as "no unique work, safe to delete".
    pub unique_commits: Option<u32>,
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

pub fn run(
    cfg: &Config,
    targets: &[String],
    opts: &RemoveOpts,
    emitter: &mut Emitter,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let targets = if targets.is_empty() {
        match paths::detect_number(&cwd, &cfg.runtime.stem) {
            Some(n) => {
                // C5: no-arg removal targets the current workspace — confirm
                // interactively (the original prompted [y/N]). --force skips it,
                // and a non-tty (scripts/cleanup handoff) proceeds without asking.
                if !opts.force && std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                    let name = cwd
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let ok = inquire::Confirm::new(&format!("Remove workspace {n} ({name})?"))
                        .with_default(false)
                        .prompt()
                        .unwrap_or(false);
                    if !ok {
                        println!("Aborted.");
                        return Ok(());
                    }
                }
                vec![n.to_string()]
            }
            None => anyhow::bail!(
                "`cw remove` requires one or more targets unless run from inside a numbered workspace"
            ),
        }
    } else {
        targets.to_vec()
    };
    let mut plans = Vec::new();
    for t in &targets {
        let r = match resolve::resolve(cfg, &cwd, Some(t)) {
            Ok(r) => r,
            Err(e) if opts.force => match force_orphan_resolution(cfg, t) {
                Some(r) => r,
                None => return Err(e),
            },
            Err(e) => return Err(e),
        };
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

    // --force is "just nuke it": skip safety checks AND the verdict report
    // entirely (the original made ZERO gh calls under FORCE and went straight to
    // removal — this keeps `cw remove --force` offline-capable for numeric
    // targets). Otherwise run the parallel safety checks + print the report.
    let verdicts: Vec<Verdict> = if opts.force {
        Vec::new()
    } else {
        std::thread::scope(|s| {
            let handles: Vec<_> = plans
                .iter_mut()
                .map(|p| s.spawn(|| safety_check(cfg, p)))
                .collect();
            for h in handles {
                let _ = h.join();
            }
        });

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
        verdicts
    };

    if opts.dry_run {
        println!("{} --dry-run; not removing", "·".dimmed());
        return Ok(());
    }

    let mut closed_tab_target = false;
    for (i, p) in plans.iter().enumerate() {
        // Non-force: skip workspaces the safety check flagged DIRTY. Force: the
        // verdicts vec is empty and every workspace is removed unconditionally.
        if !opts.force && matches!(verdicts.get(i), Some(Verdict::Dirty(_))) {
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

        // Salvage this workspace's Claude memories into the surviving worktrees
        // before deletion (opt-in via `[claude] memory_merge`), so they aren't
        // lost with the worktree.
        crate::memory::salvage_before_remove(cfg, &p.dir);

        // Drop DBs in parallel.
        drop_databases(&p.databases);

        let delete_branch = branch_is_safe_to_delete(p);

        // Remove worktree. Critical: if we're in cwd, git worktree commands
        // would fail after the dir is gone — cd to / first.
        if let Err(e) = remove_workspace_dir(cfg, p, delete_branch) {
            eprintln!("{} #{} removal failed: {:#}", "✗".red(), p.number, e);
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

fn force_orphan_resolution(cfg: &Config, target: &str) -> Option<resolve::Resolved> {
    let n = target.parse::<u32>().ok()?;
    if n == 0 || n > cfg.workspace.max_count.unwrap_or(99) {
        return None;
    }
    let root = cfg.runtime.repo_root.as_deref()?;
    let parent = root.parent()?;
    let sibling = parent.join(format!("{}_{}", cfg.runtime.stem, n));
    let tmp = Path::new("/tmp").join(format!("{}_{}", cfg.runtime.stem, n));
    let dir = if tmp.is_dir() { tmp } else { sibling };
    Some(resolve::Resolved {
        dir,
        number: Some(n),
        branch: None,
        pr: None,
    })
}

fn database_names_for(cfg: &Config, n: u32) -> Vec<String> {
    let Some(db) = &cfg.databases else {
        return Vec::new();
    };
    expand_db_names(db, n)
}

pub fn expand_db_names(db: &DatabasesCfg, n: u32) -> Vec<String> {
    // Safety: a pattern without `{n}` expands to the SAME name for every
    // workspace, so dropping it would destroy a database shared across all
    // workspaces. Refuse rather than guess.
    if !db.pattern.contains("{n}") {
        eprintln!(
            "{} [databases].pattern {:?} has no {{n}} — refusing to drop (would target a shared database)",
            "⚠".yellow(),
            db.pattern
        );
        return Vec::new();
    }
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
        unique_commits: None,
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
        // I2: detached HEAD (common in legacy --tmp/swarm worktrees). Resolve a
        // remote branch whose tip is HEAD and consult its PR, so a merged/closed
        // PR still lets `cw cleanup` sweep it. Without this, detached worktrees
        // were always DIRTY and never removed.
        if let Some(pr) = detached_head_pr(&p.dir) {
            p.pr = Some(pr);
            p.pr_state = pr_state(&p.dir, pr);
        }
        return;
    };
    let base = effective_base(&p.dir, &cfg.runtime.base_branch);
    p.unique_commits = commits_ahead(&p.dir, &base, branch);
    // Probe for an active session unconditionally: both the "no unique work"
    // verdict AND the stale/open-PR override (see verdict()) consult
    // active_session, so it must be computed whenever the workspace has a
    // branch — not only on the unique_commits == Some(0) path. Otherwise the
    // stale override removes a workspace someone is actively working in.
    p.active_session = has_active_session(&p.dir);
    if let Some(pr) = p.pr {
        p.pr_state = pr_state(&p.dir, pr);
    }
}

fn verdict(p: &Plan, opts: &RemoveOpts) -> Verdict {
    if p.uncommitted {
        return Verdict::Dirty("uncommitted changes".into());
    }
    // No unique commits vs base: clean unless a shell sits inside it. A `None`
    // (couldn't determine) deliberately falls through to the PR-state path,
    // which defaults to DIRTY — never delete on an unknown merge status.
    if p.branch.is_some() && p.unique_commits == Some(0) {
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
                // stale_hours == 0 disables the inactivity override (a 0
                // threshold must NOT auto-clean open/draft PRs — h >= 0 is always
                // true). Mirrors the original's `[[ $STALE_HOURS -gt 0 ]]` gate.
                if let Some(stale) = opts.stale_hours.filter(|s| *s > 0) {
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

/// The branch is safe to delete only when its work survives elsewhere: fully
/// merged into base, or its PR is merged/closed. A stale-override removal of an
/// open/draft-PR workspace keeps the branch — the worktree goes, the unmerged
/// work stays. Never delete on an unknown (`None`) merge status.
fn branch_is_safe_to_delete(p: &Plan) -> bool {
    p.unique_commits == Some(0)
        || matches!(p.pr_state, Some(PrState::Merged) | Some(PrState::Closed))
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
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
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

/// Prefer the remote-tracking base (`origin/<base>`) when it exists — matching
/// the original `remove-workspace.sh`, which compared against `origin/develop`
/// so a stale *local* trunk can't make merged work look unmerged (or vice
/// versa). Falls back to the local base ref when there's no remote.
fn effective_base(dir: &Path, base: &str) -> String {
    let remote = format!("origin/{base}");
    let exists = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/remotes/{remote}"))
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if exists {
        remote
    } else {
        base.to_string()
    }
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

/// PIDs of non-ancestor processes whose cwd is rooted in `dir`. Uses
/// `lsof -d cwd` and excludes our own process-tree ancestors so the process
/// running the check never counts itself. Shared by the active-session check
/// and teardown's process kill.
fn rooted_pids(dir: &Path) -> Vec<u32> {
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
        return Vec::new();
    };
    let dir_s = dir.to_string_lossy();
    let dir_prefix = format!("{}/", dir_s);
    let mut pids = Vec::new();
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
        if !ancestors.contains(&lpid) && !pids.contains(&lpid) {
            pids.push(lpid);
        }
    }
    pids
}

/// Return true if a non-ancestor process has its cwd rooted in `dir`.
fn has_active_session(dir: &Path) -> bool {
    !rooted_pids(dir).is_empty()
}

/// For a detached HEAD, find a remote branch (`origin/*`) whose tip is HEAD and
/// return its PR number, mirroring remove-workspace.sh's detached-HEAD path.
fn detached_head_pr(dir: &Path) -> Option<u32> {
    let out = Command::new("git")
        .args([
            "for-each-ref",
            "--points-at=HEAD",
            "--format=%(refname:short)",
            "refs/remotes/origin/",
        ])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .trim()
        .strip_prefix("origin/")?
        .to_string();
    crate::git::github::pr_for_branch(dir, &branch)
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
    let st = Command::new("dropdb").args(["--if-exists", name]).status();
    match st {
        Ok(s) if s.success() => println!("{} dropdb {}", "·".dimmed(), name),
        _ => {}
    }
}

/// Stop every configured service for the workspace being removed, best-effort.
/// Only services with a port+start build a `Ctx`; `processes::stop` then kills
/// by pid-file / stop_patterns / `lsof -ti:PORT`, independent of the runtime's
/// command name (so a Django/Flask backend is actually stopped).
fn stop_workspace_services(cfg: &Config, p: &Plan) {
    if cfg.services.is_empty() {
        return;
    }
    let resolved = resolve::Resolved {
        dir: p.dir.clone(),
        number: Some(p.number),
        branch: p.branch.clone(),
        pr: p.pr,
    };
    for svc in &cfg.services {
        if let Ok(ctx) = crate::serve::processes::Ctx::build(cfg, &resolved, svc) {
            let _ = crate::serve::processes::stop(&ctx);
        }
    }
}

fn run_pre_remove_hook(cfg: &Config, p: &Plan) -> Result<()> {
    let Some(hook) = &cfg.hooks.pre_remove else {
        return Ok(());
    };
    let current_dir = if p.dir.is_dir() {
        p.dir.as_path()
    } else {
        cfg.runtime.repo_root.as_deref().unwrap_or(p.dir.as_path())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(unique: Option<u32>, pr_state: Option<PrState>, active: bool) -> Plan {
        Plan {
            number: 3,
            dir: PathBuf::from("/tmp/x_3"),
            branch: Some("feature".into()),
            pr: pr_state.map(|_| 7),
            pr_state,
            uncommitted: false,
            unique_commits: unique,
            active_session: active,
            inactive_hours: None,
            databases: Vec::new(),
            head_short: None,
            is_cwd: false,
        }
    }

    // A2: a failed merge-status lookup (None) must never read as "no unique
    // work" — the workspace stays DIRTY rather than getting deleted.
    #[test]
    fn unknown_merge_status_is_dirty_not_clean() {
        assert!(matches!(
            verdict(&plan(None, None, false), &RemoveOpts::default()),
            Verdict::Dirty(_)
        ));
    }

    #[test]
    fn no_unique_commits_no_session_is_clean() {
        assert!(matches!(
            verdict(&plan(Some(0), None, false), &RemoveOpts::default()),
            Verdict::Clean(_)
        ));
    }

    #[test]
    fn no_unique_commits_but_active_session_is_dirty() {
        assert!(matches!(
            verdict(&plan(Some(0), None, true), &RemoveOpts::default()),
            Verdict::Dirty(_)
        ));
    }

    #[test]
    fn unique_commits_no_pr_is_dirty() {
        assert!(matches!(
            verdict(&plan(Some(5), None, false), &RemoveOpts::default()),
            Verdict::Dirty(_)
        ));
    }

    // A5: merged-PR removal is clean AND the branch is safe to delete.
    #[test]
    fn merged_pr_is_clean_and_branch_deletable() {
        let p = plan(Some(5), Some(PrState::Merged), false);
        assert!(matches!(
            verdict(&p, &RemoveOpts::default()),
            Verdict::Clean(_)
        ));
        assert!(branch_is_safe_to_delete(&p));
    }

    // A5: a stale-override removal of an open-PR workspace is clean (worktree
    // freed) but the branch is KEPT — it still carries unmerged work.
    #[test]
    fn stale_open_pr_is_clean_but_branch_kept() {
        let opts = RemoveOpts {
            stale_hours: Some(48),
            ..RemoveOpts::default()
        };
        let mut p = plan(Some(5), Some(PrState::Open), false);
        p.inactive_hours = Some(72);
        assert!(matches!(verdict(&p, &opts), Verdict::Clean(_)));
        assert!(
            !branch_is_safe_to_delete(&p),
            "open-PR stale removal must keep the branch"
        );
    }

    #[test]
    fn unknown_merge_status_branch_not_deletable() {
        assert!(!branch_is_safe_to_delete(&plan(None, None, false)));
    }

    // A4: a DB pattern lacking {n} must refuse to expand (would drop a shared DB).
    #[test]
    fn db_pattern_without_n_refuses() {
        let db = DatabasesCfg {
            pattern: "app_db".into(),
            suffixes: vec!["qa".into()],
            clone: "postgres".into(),
            default_source_suffix: "qa".into(),
            post_clone: None,
        };
        assert!(expand_db_names(&db, 3).is_empty());
    }

    #[test]
    fn db_pattern_with_n_expands() {
        let db = DatabasesCfg {
            pattern: "app_{n}_{suffix}".into(),
            suffixes: vec!["qa".into(), "stg".into()],
            clone: "postgres".into(),
            default_source_suffix: "qa".into(),
            post_clone: None,
        };
        assert_eq!(expand_db_names(&db, 3), vec!["app_3_qa", "app_3_stg"]);
    }
}

fn remove_workspace_dir(cfg: &Config, p: &Plan, delete_branch: bool) -> Result<()> {
    // Resolve the main worktree BEFORE any deletion or cwd change. In the
    // {stem}_{N} sibling layout the workspace's *parent* is a plain directory,
    // not a git repo, so querying from there (the old behavior) failed and all
    // git commands silently ran from `/` — leaving orphan .git/worktrees
    // metadata. Query from a known-good location instead: the repo root we were
    // invoked in, then the doomed worktree itself (still present at this point).
    let main_worktree = cfg
        .runtime
        .repo_root
        .as_deref()
        .and_then(crate::git::worktree::main_worktree)
        .or_else(|| crate::git::worktree::main_worktree(&p.dir));

    // Stop the workspace's configured dev servers first. The cwd-rooted kill
    // below only matches shells/node by command name, so a Python/Ruby/etc.
    // backend would survive (lingering, still bound to its port). processes::stop
    // kills by pid-file, port-scoped patterns, AND `lsof -ti:PORT` — command-
    // agnostic — releasing the port and file handles before removal.
    stop_workspace_services(cfg, p);

    // Kill remaining processes whose cwd is rooted in the workspace (shells,
    // `tail -f`) so they release file handles. Precise — by cwd, not a
    // command-line substring match that could hit unrelated processes.
    for pid in rooted_pids(&p.dir) {
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }

    // If cwd is inside the target, step out before running git commands.
    if p.is_cwd {
        std::env::set_current_dir("/")?;
    }

    // Purge heavy untracked dirs so `git worktree remove` doesn't choke.
    for heavy in ["node_modules", ".venv", "dist", "build"] {
        let _ = std::fs::remove_dir_all(p.dir.join(heavy));
    }

    let run_in = main_worktree.as_deref().unwrap_or_else(|| Path::new("/"));

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

    // Delete the branch only when its work is preserved elsewhere (merged/closed
    // PR, or no commits beyond base). For a stale-but-open-PR removal the branch
    // carries unmerged work — keep it; the worktree goes, the branch stays.
    if delete_branch {
        if let Some(branch) = &p.branch {
            let _ = Command::new("git")
                .args(["branch", "-D", branch])
                .current_dir(run_in)
                .output();
        }
    }

    Ok(())
}
