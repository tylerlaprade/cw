//! Workspace creation: claim a number, add worktree, copy/strip envs,
//! kick off background setup.

use crate::config::{
    schema::{EnvInject, EnvStrip},
    Config,
};
use crate::exec::detach;
use crate::util::slugify::slugify;
use anyhow::{Context, Result};
use regex::Regex;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

/// Lock dirs older than this are treated as crashed/abandoned and reclaimed.
const STALE_LOCK_AGE: Duration = Duration::from_secs(60);

/// RAII guard for a `.devcli_{stem}_{n}_claim` lock dir (created under the
/// repo's git dir). Removes the lock when dropped so the slot doesn't leak
/// past the end of `create()`.
/// Without this, Rust cw used to leak lock dirs forever — every run masked
/// the slot it claimed, so subsequent runs skipped past it and new workspaces
/// piled up at the top of the range instead of filling the gaps.
#[derive(Debug)]
pub struct LockGuard {
    path: PathBuf,
    released: bool,
}

impl LockGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            released: false,
        }
    }

    pub fn release(mut self) {
        let _ = std::fs::remove_dir(&self.path);
        self.released = true;
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if !self.released {
            let _ = std::fs::remove_dir(&self.path);
        }
    }
}

#[derive(Debug, Clone)]
pub struct CreateOpts {
    /// Either a bare branch name (already a valid git ref) or a free-form
    /// description that will be slugified.
    pub subject: String,
    /// When true, parent = current branch (Graphite stacked). Default: parent
    /// = base_branch.
    pub stack: bool,
    /// Optional parent override (branch name). When set, overrides `stack`.
    pub parent: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CreateResult {
    pub number: u32,
    pub dir: PathBuf,
    pub branch: String,
}

/// Build a branch name. If `subject` is already a plausible git ref
/// (contains only [A-Za-z0-9/_-]+ and is ≤ 100 chars), use it verbatim;
/// otherwise slugify.
pub fn branch_for_subject(subject: &str) -> String {
    let looks_like_ref = subject.len() <= 100
        && subject
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '/' || c == '.');
    if looks_like_ref && !subject.is_empty() {
        subject.to_string()
    } else {
        slugify(subject)
    }
}

pub fn create(cfg: &Config, cwd: &Path, opts: CreateOpts) -> Result<CreateResult> {
    let root = cfg
        .runtime
        .repo_root
        .as_deref()
        .context("not inside a git repo")?;
    let parent_dir = root.parent().context("repo root has no parent")?;

    let branch = branch_for_subject(&opts.subject);
    if branch.is_empty() {
        anyhow::bail!("empty branch name");
    }

    // Parent for Graphite: either provided, or current branch (--stack), or base.
    let parent_branch = if let Some(p) = opts.parent {
        p
    } else if opts.stack {
        current_branch_in(cwd).context("--stack requires a current branch")?
    } else {
        cfg.runtime.base_branch.clone()
    };

    // Claim locks live in the repo's git dir (shared across all worktrees), so
    // concurrent `cw` runs in THIS repo coordinate while different repos — even
    // with the same stem — never block each other. Tests isolate naturally,
    // each using its own temp repo.
    let lock_dir = claim_lock_dir(root);
    let (number, _lock) = claim_number(cfg, parent_dir, &lock_dir)?;
    let dir = parent_dir.join(format!("{}_{}", cfg.runtime.stem, number));
    let existed = ensure_branch_for_worktree(root, &branch)?;

    eprintln!("Creating workspace {number}...");
    eprintln!("Creating worktree at {}...", dir.display());
    add_worktree(root, &dir, &branch, &parent_branch, existed)?;
    if !existed && graphite_enabled(cfg) {
        gt_track(&dir, &parent_branch)?;
    }

    copy_envs(root, &dir, cfg)?;
    strip_envs(&dir, cfg, number)?;
    inject_envs(&dir, cfg, number)?;

    let setup_log = PathBuf::from(format!("/tmp/{}_{}_setup.log", cfg.runtime.stem, number));
    let _ = std::fs::write(
        &setup_log,
        format!("# cw setup log for {} #{}\n", branch, number),
    );
    // The DB-clone source is the workspace cw is run from (0 = main repo).
    let src_number = crate::util::paths::detect_number(cwd, &cfg.runtime.stem).unwrap_or(0);
    kick_off_setup(&dir, cfg, &setup_log, number, existed, src_number)?;
    eprintln!("Background setup running (log: {})", setup_log.display());

    // Seed the new workspace with sibling worktrees' Claude memories (opt-in via
    // `[claude] memory_merge`) so a fresh checkout's agent isn't amnesiac.
    crate::memory::seed_new_workspace(cfg, &dir);

    print_ready_banner(cfg, number, &dir, &setup_log);

    Ok(CreateResult {
        number,
        dir,
        branch,
    })
}

fn print_ready_banner(cfg: &Config, number: u32, dir: &Path, setup_log: &Path) {
    eprintln!();
    eprintln!("========================================");
    eprintln!("Workspace {number} ready!");
    eprintln!("========================================");
    eprintln!("  Directory: {}", dir.display());
    for svc in &cfg.services {
        let Some(port_cfg) = &svc.port else {
            continue;
        };
        let port = u32::from(port_cfg.base) + number;
        // Title-case the configured service name — works for ANY service, not
        // just the autodetected "frontend"/"backend".
        let mut label = svc.name.clone();
        if let Some(first) = label.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        eprintln!("  {label}: http://localhost:{port}");
    }
    if let Some(db) = &cfg.databases {
        let prefix = db
            .pattern
            .replace("{n}", &number.to_string())
            .replace("{suffix}", &db.default_source_suffix);
        eprintln!("  Database:  {prefix}");
    }
    eprintln!();
    // Tool-native instruction — `./serve.sh` is the company script and does not
    // exist in a generic repo.
    eprintln!("Start with: cw open {number}");
    eprintln!();
    eprintln!("⚠ Background setup still running.");
    eprintln!("  Tail progress: tail -f {}", setup_log.display());
    eprintln!("  Wait for SETUP_DONE before starting services.");
}

/// Reserve the lowest-available workspace number and hold a `LockGuard` for
/// it until the caller drops the guard. Mirrors Bash `find_next_number` +
/// `claim_workspace_number` in `new-workspace.sh`: consider sibling dirs,
/// `git worktree list`, and live per-slot locks (in `lock_dir`) as "in use";
/// reclaim lock dirs older than `STALE_LOCK_AGE`. `lock_dir` is the repo's git
/// dir in production so claims coordinate per-repo, not via a shared /tmp.
pub fn claim_number(cfg: &Config, parent: &Path, tmp_dir: &Path) -> Result<(u32, LockGuard)> {
    let max = cfg.workspace.max_count.unwrap_or(99);
    let stem = cfg.runtime.stem.clone();
    let repo_root = cfg.runtime.repo_root.clone();

    // Bound the race retry loop so we can't spin forever if every slot is
    // genuinely taken (another process keeps winning races).
    for _ in 0..max.saturating_add(1) {
        let used = scan_used_numbers(&stem, parent, tmp_dir, repo_root.as_deref());
        let Some(n) = (1..=max).find(|n| !used.contains(n)) else {
            anyhow::bail!(
                "no free workspace number ≤ {max} — free a slot with `cw cleanup` or `cw remove <N>`"
            );
        };
        let lock = tmp_dir.join(format!(".devcli_{}_{}_claim", stem, n));
        match std::fs::create_dir(&lock) {
            Ok(_) => return Ok((n, LockGuard::new(lock))),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Lost the race, or the lock is stale and wasn't pruned on
                // this pass. The next scan calls reclaim-if-stale and either
                // removes it or records it as live.
                continue;
            }
            Err(e) => {
                return Err(e).with_context(|| format!("creating lock dir {}", lock.display()));
            }
        }
    }
    anyhow::bail!(
        "no free workspace number ≤ {max} (races exhausted) — free a slot with `cw cleanup` or `cw remove <N>`"
    );
}

fn scan_used_numbers(
    stem: &str,
    parent: &Path,
    tmp_dir: &Path,
    repo_root: Option<&Path>,
) -> BTreeSet<u32> {
    let mut used = BTreeSet::new();
    collect_stem_numbers(parent, stem, &mut used);
    collect_stem_numbers(tmp_dir, stem, &mut used);
    collect_live_lock_numbers(tmp_dir, stem, &mut used);
    if let Some(root) = repo_root {
        collect_worktree_numbers(root, stem, &mut used);
    }
    used
}

fn collect_stem_numbers(dir: &Path, stem: &str, out: &mut BTreeSet<u32>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(n) = parse_stem_number(name, stem) {
            out.insert(n);
        }
    }
}

fn collect_worktree_numbers(inside: &Path, stem: &str, out: &mut BTreeSet<u32>) {
    let Ok(worktrees) = crate::git::worktree::list(inside) else {
        return;
    };
    for w in worktrees {
        if let Some(name) = w.dir.file_name().and_then(|n| n.to_str()) {
            if let Some(n) = parse_stem_number(name, stem) {
                out.insert(n);
            }
        }
    }
}

fn collect_live_lock_numbers(tmp_dir: &Path, stem: &str, out: &mut BTreeSet<u32>) {
    let prefix = format!(".devcli_{}_", stem);
    let suffix = "_claim";
    let Ok(entries) = std::fs::read_dir(tmp_dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(num_str) = name
            .strip_prefix(&prefix)
            .and_then(|s| s.strip_suffix(suffix))
        else {
            continue;
        };
        let Ok(n) = num_str.parse::<u32>() else {
            continue;
        };
        if lock_is_stale(&path) {
            let _ = std::fs::remove_dir(&path);
        } else {
            out.insert(n);
        }
    }
}

fn parse_stem_number(name: &str, stem: &str) -> Option<u32> {
    let rest = name.strip_prefix(stem)?.strip_prefix('_')?;
    rest.parse::<u32>().ok()
}

fn lock_is_stale(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return true;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(mtime)
        .map(|age| age > STALE_LOCK_AGE)
        .unwrap_or(false)
}

pub(crate) fn claim_lock_dir(root: &Path) -> PathBuf {
    let out = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(root)
        .output();
    match out {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s.is_empty() {
                root.join(".git")
            } else {
                PathBuf::from(s)
            }
        }
        _ => root.join(".git"),
    }
}

pub fn branch_exists(inside: &Path, branch: &str) -> Result<bool> {
    if local_branch_exists(inside, branch)? || remote_tracking_branch_exists(inside, branch)? {
        return Ok(true);
    }
    Ok(remote_head_exists(inside, branch))
}

fn ensure_branch_for_worktree(inside: &Path, branch: &str) -> Result<bool> {
    if local_branch_exists(inside, branch)? {
        return Ok(true);
    }

    fetch_branch(inside, branch);

    if remote_tracking_branch_exists(inside, branch)? {
        let out = Command::new("git")
            .args(["branch", branch])
            .arg(format!("origin/{branch}"))
            .current_dir(inside)
            .output()
            .with_context(|| format!("creating local branch {branch} from origin/{branch}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!(
                "git branch {branch} origin/{branch} failed: {}",
                stderr.trim()
            );
        }
        return Ok(true);
    }

    Ok(false)
}

fn local_branch_exists(inside: &Path, branch: &str) -> Result<bool> {
    let out = Command::new("git")
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/heads/{}", branch))
        .current_dir(inside)
        .status()?;
    Ok(out.success())
}

fn remote_tracking_branch_exists(inside: &Path, branch: &str) -> Result<bool> {
    let out = Command::new("git")
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/remotes/origin/{}", branch))
        .current_dir(inside)
        .status()?;
    Ok(out.success())
}

fn remote_head_exists(inside: &Path, branch: &str) -> bool {
    let has_origin = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(inside)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_origin {
        return false;
    }
    Command::new("git")
        .args(["ls-remote", "--exit-code", "--heads", "origin", branch])
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(inside)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn fetch_branch(inside: &Path, branch: &str) {
    let has_origin = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(inside)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_origin {
        return;
    }
    let refspec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
    let fetched = Command::new("git")
        .args(["fetch", "origin", &refspec])
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(inside)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !fetched {
        let _ = Command::new("git")
            .args(["fetch", "origin", branch])
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(inside)
            .output();
    }
}

fn add_worktree(
    inside: &Path,
    dir: &Path,
    branch: &str,
    parent_branch: &str,
    existed: bool,
) -> Result<()> {
    let mut cmd = Command::new("git");
    cmd.current_dir(inside).args(["worktree", "add"]);
    if existed {
        cmd.arg(dir).arg(branch);
    } else {
        cmd.args(["-b", branch]).arg(dir).arg(parent_branch);
    }
    // Capture stderr (not inherit) so the caller can detect git's
    // "is already used by worktree at '<path>'" message and recover by
    // switching into that worktree (busy-worktree → --continue).
    let out = cmd
        .output()
        .with_context(|| format!("git worktree add {}", dir.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprint!("{stderr}");
        anyhow::bail!("git worktree add failed: {}", stderr.trim());
    }
    Ok(())
}

fn graphite_enabled(cfg: &Config) -> bool {
    cfg.integrations.graphite.unwrap_or_else(|| in_path("gt"))
}

fn in_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p).any(|d| {
                let cand = d.join(bin);
                cand.is_file()
            })
        })
        .unwrap_or(false)
}

fn gt_track(dir: &Path, parent_branch: &str) -> Result<()> {
    let st = Command::new("gt")
        .args(["track", "--parent", parent_branch])
        .current_dir(dir)
        .status();
    if let Err(e) = st {
        eprintln!("warn: gt track failed: {e:#}");
    }
    Ok(())
}

fn copy_envs(src: &Path, dst: &Path, cfg: &Config) -> Result<()> {
    let files = if cfg.env.copy.is_empty() {
        autodetect_env_files(src)
    } else {
        cfg.env.copy.clone()
    };
    for rel in files {
        let from = src.join(&rel);
        if !from.is_file() {
            continue;
        }
        let to = dst.join(&rel);
        if let Some(p) = to.parent() {
            // H8: propagate the mkdir error instead of .ok() — otherwise the
            // copy below fails with a less informative error (or silently leaves
            // a half-populated workspace).
            std::fs::create_dir_all(p)
                .with_context(|| format!("creating env dir {}", p.display()))?;
        }
        std::fs::copy(&from, &to)
            .with_context(|| format!("copy {} → {}", from.display(), to.display()))?;
    }
    Ok(())
}

pub(crate) fn autodetect_env_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    // `.claude/settings.local.json` carries per-checkout Claude permissions; the
    // original copied it so a new workspace inherits them instead of re-prompting.
    for name in [".env", ".env.local", ".claude/settings.local.json"] {
        if root.join(name).is_file() {
            out.push(name.to_string());
        }
    }
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.filter_map(Result::ok) {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let name = match p.file_name() {
                Some(n) => n.to_string_lossy().into_owned(),
                None => continue,
            };
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            for sub in [".env", ".env.local"] {
                if p.join(sub).is_file() {
                    out.push(format!("{}/{}", name, sub));
                }
            }
        }
    }
    out
}

fn strip_envs(dst: &Path, cfg: &Config, _number: u32) -> Result<()> {
    for rule in &cfg.env.strip {
        apply_strip(&dst.join(&rule.file), &rule.patterns)
            .with_context(|| format!("stripping {}", rule.file))?;
    }
    Ok(())
}

fn apply_strip(path: &Path, patterns: &[String]) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    let text = std::fs::read_to_string(path)?;
    let res: Vec<Regex> = patterns
        .iter()
        .map(|p| Regex::new(p).with_context(|| format!("bad regex {p:?}")))
        .collect::<Result<_>>()?;
    let filtered: Vec<&str> = text
        .lines()
        .filter(|line| !res.iter().any(|r| r.is_match(line)))
        .collect();
    let mut out = filtered.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    std::fs::write(path, out)?;
    Ok(())
}

fn inject_envs(dst: &Path, cfg: &Config, number: u32) -> Result<()> {
    // H6: {port} is documented in the schema + init scaffold but was never
    // substituted. Resolve it to the first configured service's port
    // (base + number) — the common single-service case. Multi-service repos
    // that need a specific port should reference it via that service's config.
    let port = cfg
        .services
        .iter()
        .find_map(|s| s.port.as_ref())
        .map(|p| u32::from(p.base) + number);
    for rule in &cfg.env.inject {
        let path = dst.join(&rule.file);
        let mut line = rule
            .line
            .replace("{n}", &number.to_string())
            .replace("{stem}", &cfg.runtime.stem);
        if let Some(port) = port {
            line = line.replace("{port}", &port.to_string());
        }
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        if !text.ends_with('\n') && !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&line);
        text.push('\n');
        std::fs::write(&path, text)?;
    }
    // Always inject WORKSPACE_NUMBER into any *.env files we copied, so
    // services that rely on it can find the current N without further config.
    for rel in relevant_envs(dst) {
        let path = dst.join(&rel);
        if !path.is_file() {
            continue;
        }
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        if !text.contains("\nWORKSPACE_NUMBER=") && !text.starts_with("WORKSPACE_NUMBER=") {
            if !text.ends_with('\n') && !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&format!("WORKSPACE_NUMBER={}\n", number));
            std::fs::write(&path, text)?;
        }
    }
    Ok(())
}

fn relevant_envs(dst: &Path) -> Vec<String> {
    autodetect_env_files(dst)
}

fn kick_off_setup(
    dir: &Path,
    cfg: &Config,
    log: &Path,
    number: u32,
    existed: bool,
    src_number: u32,
) -> Result<()> {
    // Each phase runs INDEPENDENTLY: a dependency-install failure must not abort
    // the post_create hook or DB clone (the original used `set +e` for exactly
    // this — H3). Phases are newline-joined and run under `bash -c` (no set -e).
    let mut phases: Vec<String> = Vec::new();

    // H2: restack an existing branch onto its parent, best-effort — mirrors
    // new-workspace.sh's `try_restack` on EXISTING_BRANCH, which deliberately
    // used `gt get` WITHOUT `--force` to avoid clobbering un-pushed local work
    // on a branch that was created elsewhere. (T1.2: cw previously forced here,
    // inverting the original's safety choice.)
    if existed && graphite_enabled(cfg) {
        phases.push(
            "gt get </dev/null >/dev/null 2>&1 && gt r --quiet </dev/null 2>&1 \
             || git rebase --abort >/dev/null 2>&1 || true"
                .into(),
        );
    }

    // Dependency installs. Configured [deps] honor `parallel`; autodetected ones
    // run concurrently (H5 — the original installed Python + JS in parallel).
    if let Some(deps) = &cfg.deps {
        let subs: Vec<String> = deps
            .install
            .iter()
            .map(|i| format!("( cd {} && {} )", shell_quote(&i.dir), i.cmd))
            .collect();
        if !subs.is_empty() {
            phases.push(if deps.parallel {
                format!("{{ {}; wait; }}", subs.join(" & "))
            } else {
                subs.join(" && ")
            });
        }
    } else {
        let installs = autodetect_dep_installs(dir);
        if !installs.is_empty() {
            phases.push(format!("{{ {}; wait; }}", installs.join(" & ")));
        }
    }

    // H1: clone per-workspace databases — opt-in via [databases] clone = "postgres".
    if let Some(db) = &cfg.databases {
        if db.clone == "postgres" {
            if let Some(snippet) = db_clone_snippet(db, src_number, number) {
                // Postgres `createdb -T` (the fast template-copy path) fails if any
                // session is connected to the SOURCE template DB — the common case
                // when branching off a running workspace, forcing the slow
                // pg_dump|psql fallback. Bounce the source workspace's dev server
                // around the clone (only if it was running), mirroring
                // new-workspace.sh's stop/restart — but per-workspace, not global.
                match source_bounce(cfg, src_number) {
                    Some((pre, post)) => phases.push(format!("{pre}\n{snippet}\n{post}")),
                    None => phases.push(snippet),
                }
            }
            // post_clone: bring the freshly-cloned DB up to the branch's schema
            // (e.g. `manage.py migrate`) once, in the workspace root. Runs after
            // the clone in the same detached chain.
            if let Some(pc) = &db.post_clone {
                let pc = pc
                    .replace("{n}", &number.to_string())
                    .replace("{stem}", &cfg.runtime.stem);
                phases.push(pc);
            }
        }
    }

    // post_create hook runs regardless of earlier phase outcomes (H3).
    if let Some(hook) = &cfg.hooks.post_create {
        phases.push(hook.clone());
    }

    if phases.is_empty() {
        // Nothing to do; mark done immediately.
        let _ = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(log)
            .and_then(|mut f| std::io::Write::write_all(&mut f, b"SETUP_DONE rc=0\n"));
        return Ok(());
    }

    // Newline-join so a failing phase doesn't abort the rest (no `&&` between
    // phases). Within a phase, `&&`/`&` semantics are preserved.
    let chain = phases.join("\n");
    // Strip UV_WORKING_DIR inherited from the caller's direnv context: if the
    // caller ran `cw` from inside another workspace's subdir, `uv run --script`
    // in a hook would chdir there and resolve `./scripts/…` against the wrong
    // tree. Mirrors `new-workspace.sh`.
    detach::spawn_shell_detached(&chain, dir, log, "SETUP_DONE", &["UV_WORKING_DIR"])?;
    Ok(())
}

/// Build a best-effort parallel DB-clone snippet: for each suffix, clone the
/// matching source DB (`pattern` filled with `src_number` + that suffix) into
/// the new workspace's DB (`pattern` filled with `dst_number` + that suffix).
/// `createdb -T` (template copy) with a `pg_dump | psql` fallback. Every clone
/// is `|| true` so a missing source never fails the whole setup. Returns None
/// when the pattern has no `{n}` (every workspace would share one DB name).
fn db_clone_snippet(
    db: &crate::config::schema::DatabasesCfg,
    src_number: u32,
    dst_number: u32,
) -> Option<String> {
    if !db.pattern.contains("{n}") || db.suffixes.is_empty() {
        return None;
    }
    let fill = |n: u32, suffix: &str| {
        db.pattern
            .replace("{n}", &n.to_string())
            .replace("{suffix}", suffix)
    };
    let clones: Vec<String> = db
        .suffixes
        .iter()
        .map(|suffix| {
            let src = fill(src_number, suffix);
            let dst = fill(dst_number, suffix);
            // Skip a no-op self-clone (src == dst).
            if src == dst {
                return "true".to_string();
            }
            // Drop any pre-existing destination DB first. `claim_number` just
            // reclaimed this workspace slot as free (no live dir/worktree/lock
            // owns it), so a DB matching the slot's name is by definition an
            // orphan from a prior workspace — without this, `createdb`/`createdb -T`
            // both fail on the existing DB, `|| true` swallows it, and the new
            // workspace silently runs against the OLD workspace's stale data.
            format!(
                "( dropdb --if-exists '{dst}' 2>/dev/null; \
                 createdb -T '{src}' '{dst}' 2>/dev/null \
                 || {{ createdb '{dst}' 2>/dev/null && pg_dump '{src}' 2>/dev/null | psql -q '{dst}' 2>/dev/null; }} \
                 || true )"
            )
        })
        .collect();
    Some(format!("{{ {}; wait; }}", clones.join(" & ")))
}

/// Build a (pre, post) shell pair that stops the SOURCE workspace's dev server
/// before a DB clone and restarts it after — but only if it was running (so we
/// never start a server the user had stopped). Returns None when there are no
/// startable services to bounce. `cw serve` (run with cwd = the source dir)
/// resolves the source by cwd and handles per-service stop/start; degrades to a
/// no-op (`|| true`) if `cw` isn't on PATH in the detached setup shell.
fn source_bounce(cfg: &Config, src_number: u32) -> Option<(String, String)> {
    let root = cfg.runtime.repo_root.as_deref()?;
    let src_dir = if src_number == 0 {
        root.to_path_buf()
    } else {
        root.parent()?
            .join(format!("{}_{}", cfg.runtime.stem, src_number))
    };
    // Source pid files for the running-check: a service is "running" if its pid
    // file exists and names a live process.
    let mut pid_files: Vec<String> = Vec::new();
    for svc in &cfg.services {
        let Some(port_cfg) = &svc.port else { continue };
        if svc.start.is_none() {
            continue;
        }
        let port = u16::try_from(u32::from(port_cfg.base).saturating_add(src_number)).ok()?;
        let pf = crate::serve::expand_template(
            svc.pid_file
                .as_deref()
                .unwrap_or("/tmp/{stem}_{n}_{svc}.pid"),
            &cfg.runtime.stem,
            src_number,
            port,
            &[("svc", &svc.name)],
        );
        pid_files.push(pf);
    }
    if pid_files.is_empty() {
        return None;
    }
    let src_q = shell_quote(&src_dir.display().to_string());
    let checks: String = pid_files
        .iter()
        .map(|pf| {
            format!(
                "if [ -f {pf} ] && kill -0 \"$(cat {pf} 2>/dev/null)\" 2>/dev/null; then __cw_src_running=1; fi",
                pf = shell_quote(pf)
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let pre = format!(
        "__cw_src_running=0; {checks}; \
         [ \"$__cw_src_running\" = 1 ] && ( cd {src_q} && cw serve stop ) >/dev/null 2>&1 || true"
    );
    let post = format!(
        "[ \"$__cw_src_running\" = 1 ] && ( cd {src_q} && cw serve start ) >/dev/null 2>&1 || true"
    );
    Some((pre, post))
}

pub(crate) fn autodetect_dep_installs(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    // Scan the repo root itself (single-package layout) plus every top-level
    // subdir (monorepo). Without the root, a single-package repo got no
    // background dependency install on workspace creation.
    let candidates = std::iter::once((root.to_path_buf(), ".".to_string())).chain(
        top_level_dirs(root).into_iter().map(|d| {
            let name = d
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| ".".into());
            (d, name)
        }),
    );
    for (entry, dirname) in candidates {
        if entry.join("pyproject.toml").is_file() && entry.join("uv.lock").is_file() {
            out.push(format!("( cd {} && uv sync )", shell_quote(&dirname)));
        } else if entry.join("bun.lock").is_file() || entry.join("bun.lockb").is_file() {
            out.push(format!("( cd {} && bun install )", shell_quote(&dirname)));
        } else if entry.join("package.json").is_file() {
            out.push(format!("( cd {} && npm install )", shell_quote(&dirname)));
        }
    }
    out
}

pub(crate) fn top_level_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(iter) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    iter.filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .map(|n| {
                        let s = n.to_string_lossy();
                        !s.starts_with('.') && s != "target" && s != "node_modules"
                    })
                    .unwrap_or(false)
        })
        .collect()
}

fn current_branch_in(dir: &Path) -> Option<String> {
    let out = Command::new("git")
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

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+@=,:".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

// Silence unused re-exports until wired in step 5.
#[allow(dead_code)]
fn _use_marker(_: &EnvStrip, _: &EnvInject) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{Config, Runtime, WorkspaceCfg};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn create_registers_new_worktree_with_gt_track() {
        let _guard = env_lock().lock().unwrap();

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("source");
        let mock_bin = temp.path().join("bin");
        let gt_log = temp.path().join("gt.log");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&mock_bin).unwrap();

        let original_path = std::env::var_os("PATH");
        let mut new_path =
            std::env::split_paths(&original_path.clone().unwrap_or_default()).collect::<Vec<_>>();
        new_path.insert(0, mock_bin.clone());
        std::env::set_var("PATH", std::env::join_paths(new_path).unwrap());

        let gt = mock_bin.join("gt");
        fs::write(
            &gt,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
                gt_log.display()
            ),
        )
        .unwrap();
        chmod_x(&gt);

        init_git_repo(&root);

        let stem = temp
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let cfg = Config {
            workspace: WorkspaceCfg {
                max_count: Some(48),
                base_branch: None,
                stem: None,
                auto_restack: false,
            },
            integrations: crate::config::schema::Integrations {
                graphite: Some(true),
                github: None,
                claude: None,
                codex: None,
                direnv: None,
                acli: None,
            },
            services: Vec::new(),
            deps: None,
            databases: None,
            restack: Default::default(),
            hooks: Default::default(),
            env: Default::default(),
            cleanup: Default::default(),
            triage: Default::default(),
            claude: Default::default(),
            runtime: Runtime {
                repo_root: Some(root.clone()),
                config_path: None,
                config_root: Some(root.clone()),
                stem: stem.clone(),
                base_branch: "develop".into(),
            },
        };

        let result = create(
            &cfg,
            &root,
            CreateOpts {
                subject: "feature/foo".into(),
                stack: false,
                parent: None,
            },
        )
        .unwrap();

        assert_eq!(result.number, 1);
        assert_eq!(result.branch, "feature/foo");
        assert_eq!(result.dir, temp.path().join(format!("{stem}_1")));
        assert!(result.dir.is_dir());
        // `contains`, not exact-equality: this test mutates the global PATH to a
        // fake `gt` whose log is shared, so a concurrently-running test that also
        // shells out to `gt` may append extra lines. The intent is only that
        // create() registered the new branch with `gt track --parent <base>`.
        let gt_calls = fs::read_to_string(&gt_log).unwrap();
        assert!(
            gt_calls.contains("track --parent develop"),
            "expected `gt track --parent develop`, got:\n{gt_calls}"
        );

        match original_path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
    }

    fn init_git_repo(root: &Path) {
        git(root, ["init", "--initial-branch=develop"]);
        git(root, ["config", "user.email", "test@example.com"]);
        git(root, ["config", "user.name", "Test User"]);
        git(root, ["config", "commit.gpgsign", "false"]);
        fs::write(root.join("README.md"), "root\n").unwrap();
        git(root, ["add", "README.md"]);
        git(root, ["commit", "-m", "root"]);
    }

    fn git<const N: usize>(root: &Path, args: [&str; N]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success(), "git {:?} failed", args);
    }

    fn chmod_x(path: &PathBuf) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut perms = fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(path, perms).unwrap();
        }
    }

    fn test_cfg(root: &Path, stem: &str, max: u32) -> Config {
        Config {
            workspace: WorkspaceCfg {
                max_count: Some(max),
                base_branch: None,
                stem: None,
                auto_restack: false,
            },
            integrations: crate::config::schema::Integrations {
                graphite: Some(false),
                github: None,
                claude: None,
                codex: None,
                direnv: None,
                acli: None,
            },
            services: Vec::new(),
            deps: None,
            databases: None,
            restack: Default::default(),
            hooks: Default::default(),
            env: Default::default(),
            cleanup: Default::default(),
            triage: Default::default(),
            claude: Default::default(),
            runtime: Runtime {
                repo_root: Some(root.to_path_buf()),
                config_path: None,
                config_root: Some(root.to_path_buf()),
                stem: stem.into(),
                base_branch: "develop".into(),
            },
        }
    }

    fn set_mtime(path: &Path, age: Duration) {
        use std::fs::FileTimes;
        let target = SystemTime::now()
            .checked_sub(age)
            .expect("age within SystemTime range");
        let times = FileTimes::new().set_accessed(target).set_modified(target);
        let f =
            std::fs::File::open(path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
        f.set_times(times)
            .unwrap_or_else(|e| panic!("set_times {}: {e}", path.display()));
    }

    fn sandbox(stem: &str) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("parent");
        let tmp = temp.path().join("tmp");
        let root = parent.join(format!("{stem}_main"));
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        init_git_repo(&root);
        (temp, parent, tmp, root)
    }

    #[test]
    fn claim_number_fills_lowest_gap() {
        let (_temp, parent, tmp, root) = sandbox("cwtest");
        std::fs::create_dir_all(parent.join("cwtest_1")).unwrap();
        std::fs::create_dir_all(parent.join("cwtest_3")).unwrap();
        let cfg = test_cfg(&root, "cwtest", 48);
        let (n, lock) = claim_number(&cfg, &parent, &tmp).unwrap();
        assert_eq!(n, 2);
        assert!(tmp.join(".devcli_cwtest_2_claim").is_dir());
        drop(lock);
        assert!(!tmp.join(".devcli_cwtest_2_claim").is_dir());
    }

    #[test]
    fn claim_number_skips_active_lock() {
        let (_temp, parent, tmp, root) = sandbox("cwtest");
        std::fs::create_dir_all(parent.join("cwtest_1")).unwrap();
        std::fs::create_dir(tmp.join(".devcli_cwtest_2_claim")).unwrap();
        let cfg = test_cfg(&root, "cwtest", 48);
        let (n, _lock) = claim_number(&cfg, &parent, &tmp).unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn claim_number_reclaims_stale_lock() {
        let (_temp, parent, tmp, root) = sandbox("cwtest");
        std::fs::create_dir_all(parent.join("cwtest_1")).unwrap();
        let stale = tmp.join(".devcli_cwtest_2_claim");
        std::fs::create_dir(&stale).unwrap();
        set_mtime(&stale, Duration::from_secs(120));
        let cfg = test_cfg(&root, "cwtest", 48);
        let (n, _lock) = claim_number(&cfg, &parent, &tmp).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn claim_number_honors_git_worktree_list() {
        let (_temp, parent, tmp, root) = sandbox("cwtest");
        let elsewhere = parent.join("cwtest_2");
        let status = Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "claimed",
                elsewhere.to_str().unwrap(),
                "develop",
            ])
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(status.success());
        let cfg = test_cfg(&root, "cwtest", 48);
        let (n, _lock) = claim_number(&cfg, &parent, &tmp).unwrap();
        assert_eq!(n, 1);

        std::fs::create_dir_all(parent.join("cwtest_1")).unwrap();
        let (n2, _lock2) = claim_number(&cfg, &parent, &tmp).unwrap();
        assert_eq!(n2, 3);
    }

    #[test]
    fn claim_number_bails_when_all_slots_taken() {
        let (_temp, parent, tmp, root) = sandbox("cwtest");
        for i in 1..=3 {
            std::fs::create_dir_all(parent.join(format!("cwtest_{i}"))).unwrap();
        }
        let cfg = test_cfg(&root, "cwtest", 3);
        let err = claim_number(&cfg, &parent, &tmp).unwrap_err();
        assert!(
            err.to_string().contains("no free workspace number"),
            "{err}"
        );
    }

    /// Regression: the pre-fix `claim_number` created a `mkdir` lock under
    /// `/tmp` and never removed it, so each completed create() permanently
    /// masked the slot it claimed. After a few runs every low number was
    /// "locked" even though the corresponding dirs were long gone, and new
    /// workspaces piled up at the top of the range. A freed slot must be
    /// reusable on the next claim.
    #[test]
    fn claim_number_does_not_leak_locks_between_claims() {
        let (_temp, parent, tmp, root) = sandbox("cwtest");
        let cfg = test_cfg(&root, "cwtest", 48);

        let (n1, guard1) = claim_number(&cfg, &parent, &tmp).unwrap();
        assert_eq!(n1, 1);
        std::fs::create_dir_all(parent.join(format!("cwtest_{n1}"))).unwrap();
        drop(guard1);
        assert!(!tmp.join(".devcli_cwtest_1_claim").exists());

        // Simulate `cw remove 1` by removing the dir; a second claim should
        // now pick 1 again — the slot is free, nothing stale should be
        // masking it.
        std::fs::remove_dir_all(parent.join("cwtest_1")).unwrap();
        let (n2, _guard2) = claim_number(&cfg, &parent, &tmp).unwrap();
        assert_eq!(n2, 1);
    }

    #[test]
    fn lock_guard_releases_on_drop() {
        let (_temp, _parent, tmp, _root) = sandbox("cwtest");
        let path = tmp.join(".devcli_cwtest_9_claim");
        std::fs::create_dir(&path).unwrap();
        {
            let _g = LockGuard::new(path.clone());
            assert!(path.is_dir());
        }
        assert!(!path.exists());
    }

    fn db_cfg(pattern: &str) -> crate::config::schema::DatabasesCfg {
        crate::config::schema::DatabasesCfg {
            pattern: pattern.into(),
            suffixes: vec!["qa".into(), "stg".into()],
            clone: "postgres".into(),
            default_source_suffix: "qa".into(),
            post_clone: None,
        }
    }

    #[test]
    fn db_clone_snippet_clones_each_suffix_from_source() {
        let s = db_clone_snippet(&db_cfg("app_{n}_{suffix}"), 0, 3).unwrap();
        // Each destination suffix clones from the source workspace's matching
        // suffix. This mirrors the legacy qa/stg/prod behavior.
        assert!(s.contains("createdb -T 'app_0_qa' 'app_3_qa'"), "{s}");
        assert!(s.contains("createdb -T 'app_0_stg' 'app_3_stg'"), "{s}");
        assert!(s.contains("pg_dump 'app_0_stg'"), "{s}");
    }

    #[test]
    fn db_clone_snippet_none_when_pattern_lacks_n() {
        // No {n} → every workspace would share one DB name; refuse to clone.
        assert!(db_clone_snippet(&db_cfg("app_{suffix}"), 0, 3).is_none());
    }
}
