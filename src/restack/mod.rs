//! `cw restack`: generic rebase loop + optional repo hook + resolver.

pub mod resolvers;

use crate::cli::{ResolveArgs, RestackArgs};
use crate::config::{self, Config};
use crate::git::github;
use crate::shell::{Emitter, Record};
use crate::workspace::{create, resolve};
use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub fn run(args: RestackArgs, emitter: &mut Emitter) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let cfg = config::discover::load(&cwd)?;
    let r = resolve_or_create(&cfg, &cwd, args.target.as_deref())?;
    let dir = r.dir.clone();
    emit_shell_state(emitter, &cwd, &r);

    // D4: never autostash when a rebase is already in progress — that's the
    // resume case, where the dirty tree IS the in-flight conflict resolution.
    // `git stash` there would strip the user's staged resolution.
    let stashed = if rebase_in_progress(&dir) {
        false
    } else {
        autostash(&dir)?
    };
    let out = run_loop(&cfg, &dir, &args);
    if stashed {
        restore_stash(&dir);
    }
    out
}

/// Resolve the restack target, falling back to workspace creation when the
/// target is an open PR (or branch) that has no worktree yet. Mirrors Bash
/// `_cw_find_or_create_workspace` in `cw.sh` for the restack path.
fn resolve_or_create(
    cfg: &Config,
    cwd: &Path,
    target: Option<&str>,
) -> Result<resolve::Resolved> {
    let err = match resolve::resolve(cfg, cwd, target) {
        Ok(r) => return Ok(r),
        Err(e) => e,
    };
    let Some(t) = target else {
        return Err(err);
    };

    let (branch, pr_num) = if let Ok(n) = t.parse::<u32>() {
        let cap = cfg.workspace.max_count.unwrap_or(99);
        if n <= cap {
            return Err(err);
        }
        let root = cfg
            .runtime
            .repo_root
            .as_deref()
            .context("no repo root discovered")?;
        let pr = match github::view_pr(root, n) {
            Ok(pr) => pr,
            Err(_) => return Err(err),
        };
        if pr.state != "OPEN" {
            anyhow::bail!(
                "PR #{n} ({}) is {} and has no workspace — create one first",
                pr.head_branch,
                pr.state.to_lowercase()
            );
        }
        println!("Found PR #{n} → {}", pr.head_branch);
        (pr.head_branch, Some(n))
    } else {
        (t.to_string(), None)
    };

    let result = create::create(
        cfg,
        cwd,
        create::CreateOpts {
            subject: branch,
            stack: false,
            parent: None,
        },
    )?;
    Ok(resolve::Resolved {
        dir: result.dir,
        number: Some(result.number),
        branch: Some(result.branch),
        pr: pr_num,
    })
}

fn run_loop(cfg: &Config, dir: &Path, args: &RestackArgs) -> Result<()> {
    // Detached worktrees are restored when this guard drops — on every exit
    // path (success, conflict-bail, error), mirroring restack.sh's EXIT trap.
    let mut detached = DetachedWorktrees::new();

    if !rebase_in_progress(dir) {
        // Kick off the rebase.
        if graphite_enabled(cfg) {
            let out = Command::new("gt")
                .args(["get", "--no-interactive"])
                .current_dir(dir)
                .output();
            match out {
                Ok(out) if !out.status.success() => {
                    eprintln!("warn: {}", command_failure("gt get failed", &out));
                }
                Err(e) => eprintln!("warn: gt get failed: {e:#}"),
                Ok(_) => {}
            }

            // D1: `gt r` can fail because a branch it needs to move is checked
            // out in a sibling worktree ("fatal: '<b>' is already used by
            // worktree at '<p>'") — the norm in the {stem}_{N} layout. Detach
            // the blocker and retry (bounded), instead of hard-bailing.
            let mut gt_r_ok = false;
            let mut last_failure = String::new();
            for _ in 0..12 {
                let out = Command::new("gt")
                    .args(["r", "--quiet"])
                    .current_dir(dir)
                    .output()?;
                if out.status.success() {
                    gt_r_ok = true;
                    break;
                }
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                if let Some((branch, path)) = parse_worktree_conflict(&stderr) {
                    println!(
                        "{} detaching {} in {} to unblock restack",
                        "→".cyan(),
                        branch,
                        path
                    );
                    detached.detach(PathBuf::from(&path), branch)?;
                    continue;
                }
                last_failure = command_failure("gt restack failed", &out);
                break;
            }
            if gt_r_ok && !rebase_in_progress(dir) {
                return finalize(cfg, dir);
            }
            if !gt_r_ok && !rebase_in_progress(dir) {
                anyhow::bail!(
                    "{}",
                    if last_failure.is_empty() {
                        "gt restack failed".to_string()
                    } else {
                        last_failure
                    }
                );
            }
        } else {
            println!("{} git rebase {}", "→".cyan(), cfg.runtime.base_branch);
            let st = Command::new("git")
                .args(["rebase", &cfg.runtime.base_branch])
                .current_dir(dir)
                .status()?;
            if st.success() {
                return finalize(cfg, dir);
            }
            // D2: a failed `git rebase` that left NO rebase in progress is a
            // real failure (bad base, precondition), not conflicts to resolve.
            // Don't fall through — the loop would print "restack complete".
            if !rebase_in_progress(dir) {
                anyhow::bail!("git rebase {} failed", cfg.runtime.base_branch);
            }
        }
    }

    // Resolution loop.
    loop {
        if !rebase_in_progress(dir) {
            return finalize(cfg, dir);
        }
        let unresolved = unresolved_files(dir)?;
        if unresolved.is_empty() {
            // Nothing unresolved but rebase still in progress — continue.
            if !try_continue(cfg, dir)? {
                return Err(anyhow::anyhow!(
                    "rebase stalled with no unresolved files (manual `gt continue` or `git rebase --continue` needed)"
                ));
            }
            continue;
        }

        println!(
            "{} {} unresolved file(s)",
            "⚠".yellow(),
            unresolved.len().bold()
        );
        for f in &unresolved {
            println!("  {}", f.display());
        }

        // 1. Run the repo hook script, if present.
        if !args.no_hook {
            if let Some(hook) = hook_path(cfg, dir) {
                run_hook(&hook, dir, &unresolved)?;
            }
        }

        stage_resolved_files(dir, &unresolved)?;
        let still = unresolved_files(dir)?;
        if !still.is_empty() {
            // 2. Fall through to the resolver.
            let resolver = pick_resolver(cfg, args);
            resolvers::run(resolver, dir, &still)?;
            stage_resolved_files(dir, &still)?;
        }

        let remaining = unresolved_files(dir)?;
        if !remaining.is_empty() {
            // Give the user a foothold — save state and bail. Re-running
            // `cw restack` picks up from here idempotently.
            eprintln!(
                "{} {} file(s) still conflict. Resolve them and re-run `cw restack`.",
                "✗".red(),
                remaining.len()
            );
            for f in &remaining {
                eprintln!("  {}", f.display());
            }
            return Err(anyhow::anyhow!("conflicts remain"));
        }

        if !try_continue(cfg, dir)? {
            // D3: `gt continue` / `git rebase --continue` failed. If a rebase is
            // still in progress, the NEXT commit conflicts — loop to resolve it.
            // If NOT, the continue genuinely failed; bail rather than fall to the
            // loop top and report a false "restack complete".
            if !rebase_in_progress(dir) {
                anyhow::bail!(
                    "`{}` failed and left no rebase in progress",
                    if graphite_enabled(cfg) {
                        "gt continue"
                    } else {
                        "git rebase --continue"
                    }
                );
            }
        }
    }
}

fn finalize(cfg: &Config, dir: &Path) -> Result<()> {
    // D5: auto-submit is opt-in. The original always ran `gt ss` (submit the
    // stack) after a restack, but that pushes branches and opens/updates PRs —
    // opinionated and requires Graphite auth, so a generic user shouldn't get
    // it by default. Enable with `[restack] submit = true`.
    if graphite_enabled(cfg) && cfg.restack.submit {
        let st = Command::new("gt")
            .args(["ss", "--no-interactive"])
            .current_dir(dir)
            .status();
        if !matches!(st, Ok(s) if s.success()) {
            eprintln!(
                "{} stack submit failed — submit manually with `gt ss`",
                "⚠".yellow()
            );
        }
    }
    println!("{} restack complete", "✓".green());
    Ok(())
}

/// Worktrees temporarily detached so `gt r` could move their branch. Restored
/// (re-checkout the branch) when this guard drops — covering every exit path,
/// like restack.sh's `restore_worktrees` EXIT trap.
struct DetachedWorktrees(Vec<(PathBuf, String)>);

impl DetachedWorktrees {
    fn new() -> Self {
        Self(Vec::new())
    }

    fn detach(&mut self, path: PathBuf, branch: String) -> Result<()> {
        let st = Command::new("git")
            .args(["checkout", "--detach", "--quiet"])
            .current_dir(&path)
            .status()
            .with_context(|| format!("detaching worktree at {}", path.display()))?;
        if !st.success() {
            anyhow::bail!("failed to detach {} in {}", branch, path.display());
        }
        self.0.push((path, branch));
        Ok(())
    }
}

impl Drop for DetachedWorktrees {
    fn drop(&mut self) {
        for (path, branch) in &self.0 {
            let ok = Command::new("git")
                .args(["checkout", branch, "--quiet"])
                .current_dir(path)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                eprintln!(
                    "{} could not restore {} in {} — run: git -C {} checkout {}",
                    "⚠".yellow(),
                    branch,
                    path.display(),
                    path.display(),
                    branch
                );
            }
        }
    }
}

/// Parse git's "fatal: '<branch>' is already used by worktree at '<path>'".
fn parse_worktree_conflict(stderr: &str) -> Option<(String, String)> {
    let re = regex::Regex::new(r"'([^']+)' is already used by worktree at '([^']+)'").ok()?;
    let caps = re.captures(stderr)?;
    Some((
        caps.get(1)?.as_str().to_string(),
        caps.get(2)?.as_str().to_string(),
    ))
}

// --- helpers --------------------------------------------------------------

fn emit_shell_state(emitter: &mut Emitter, cwd: &Path, r: &resolve::Resolved) {
    if r.dir == cwd {
        return;
    }

    let cd = r.dir.to_string_lossy().to_string();
    emitter.emit(Record::Cd(&cd));
    if let Some(n) = r.number {
        if n != 0 {
            let title = format!("#{n}");
            emitter.emit(Record::Title(&title));
        }
    }
}

fn command_failure(prefix: &str, out: &Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let detail = if !stderr.is_empty() && !stdout.is_empty() {
        format!("{stderr}\n{stdout}")
    } else if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit {}", out.status.code().unwrap_or(-1))
    };
    format!("{prefix}: {detail}")
}

pub fn rebase_in_progress(dir: &Path) -> bool {
    // git uses .git/rebase-merge or .git/rebase-apply. In a worktree, GIT_DIR
    // points into .git/worktrees/<name>/; use rev-parse to find it.
    let git_dir = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim().to_string()));
    let Some(git_dir) = git_dir else {
        return false;
    };
    let base = if git_dir.is_absolute() {
        git_dir
    } else {
        dir.join(git_dir)
    };
    base.join("rebase-merge").exists() || base.join("rebase-apply").exists()
}

pub fn unresolved_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let out = Command::new("git")
        .args(["diff", "--name-only", "--diff-filter=U"])
        .current_dir(dir)
        .output()
        .context("git diff --diff-filter=U")?;
    if !out.status.success() {
        anyhow::bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect())
}

fn stage_resolved_files(dir: &Path, candidates: &[PathBuf]) -> Result<()> {
    let mut resolved = Vec::new();
    for rel in candidates {
        if !conflict_markers_present(&dir.join(rel))? {
            resolved.push(rel.clone());
        }
    }
    if resolved.is_empty() {
        return Ok(());
    }

    let st = Command::new("git")
        .arg("add")
        .arg("--")
        .args(resolved.iter().map(|p| p.as_os_str()))
        .current_dir(dir)
        .status()
        .context("git add resolved files")?;
    if !st.success() {
        anyhow::bail!("git add failed while staging resolved files");
    }
    Ok(())
}

fn conflict_markers_present(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(text.lines().any(|line| {
        line.starts_with("<<<<<<<") || line.starts_with("=======") || line.starts_with(">>>>>>>")
    }))
}

fn autostash(dir: &Path) -> Result<bool> {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir)
        .output()?;
    if out.stdout.is_empty() {
        return Ok(false);
    }
    let st = Command::new("git")
        .args(["stash", "push", "-u", "-m", "cw-restack-autostash"])
        .current_dir(dir)
        .status()?;
    Ok(st.success())
}

fn restore_stash(dir: &Path) {
    let st = Command::new("git")
        .args(["stash", "pop"])
        .current_dir(dir)
        .status();
    if !matches!(st, Ok(s) if s.success()) {
        eprintln!(
            "{} cw-restack-autostash could not be popped cleanly; `git stash list` to recover",
            "⚠".yellow()
        );
    }
}

fn try_continue(cfg: &Config, dir: &Path) -> Result<bool> {
    // Stage any hook-touched files that the hook forgot.
    let _ = Command::new("git")
        .args(["add", "-u"])
        .current_dir(dir)
        .status();

    let argv: &[&str] = if graphite_enabled(cfg) {
        &["gt", "continue"]
    } else {
        &["git", "rebase", "--continue"]
    };
    let mut cmd = Command::new(argv[0]);
    cmd.args(&argv[1..]).current_dir(dir);
    let st = cmd.status()?;
    Ok(st.success())
}

fn hook_path(cfg: &Config, dir: &Path) -> Option<PathBuf> {
    // Hook travels with the config: look at the current worktree first (local
    // override), then the config root (where `.devcli.toml` was loaded from —
    // typically the main worktree for linked worktrees).
    let mut roots: Vec<PathBuf> = vec![dir.to_path_buf()];
    if let Some(root) = &cfg.runtime.config_root {
        if root != dir {
            roots.push(root.clone());
        }
    }
    let rel = cfg
        .restack
        .hook
        .as_deref()
        .unwrap_or("scripts/cw-restack-hook.sh");
    for root in &roots {
        let p = root.join(rel);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn run_hook(hook: &Path, dir: &Path, files: &[PathBuf]) -> Result<()> {
    println!("{} hook {}", "→".cyan(), hook.display());
    let mut cmd = Command::new(hook);
    for f in files {
        cmd.arg(f.as_os_str());
    }
    cmd.current_dir(dir);
    let st = cmd
        .status()
        .with_context(|| format!("running {}", hook.display()))?;
    if !st.success() {
        eprintln!(
            "{} hook exited {}; continuing to resolver",
            "⚠".yellow(),
            st.code().unwrap_or(-1)
        );
    }
    // Stage whatever the hook touched.
    let _ = Command::new("git")
        .args(["add", "-u"])
        .current_dir(dir)
        .status();
    Ok(())
}

fn pick_resolver(cfg: &Config, args: &RestackArgs) -> resolvers::Kind {
    resolver_from(args.resolver.as_deref(), cfg)
}

fn resolver_from(override_: Option<&str>, cfg: &Config) -> resolvers::Kind {
    if let Some(r) = override_ {
        return resolvers::Kind::parse(r);
    }
    if let Some(r) = cfg.restack.resolver.as_deref() {
        return resolvers::Kind::parse(r);
    }
    resolvers::Kind::autodetect()
}

/// Entry point for `cw resolve <files>`. Loads config, picks the configured
/// (or overridden) resolver, and runs it against `args.files` in the current
/// working directory. Intended for restack hooks that need the user's
/// resolver without hardcoding a specific CLI.
pub fn resolve_cmd(args: ResolveArgs) -> Result<()> {
    if args.files.is_empty() {
        return Ok(());
    }
    let cwd = std::env::current_dir()?;
    let cfg = config::discover::load(&cwd)?;
    let kind = resolver_from(args.resolver.as_deref(), &cfg);
    let files: Vec<PathBuf> = args.files.into_iter().map(PathBuf::from).collect();
    resolvers::run(kind, &cwd, &files)
}

#[cfg(test)]
mod tests {
    use super::{conflict_markers_present, hook_path, parse_worktree_conflict};

    #[test]
    fn parses_worktree_conflict_from_gt_stderr() {
        let stderr =
            "fatal: 'feat/foo' is already used by worktree at '/Users/me/code/app_3'\n";
        assert_eq!(
            parse_worktree_conflict(stderr),
            Some(("feat/foo".to_string(), "/Users/me/code/app_3".to_string()))
        );
        assert_eq!(parse_worktree_conflict("fatal: not a git repository\n"), None);
    }
    use crate::config::schema::Config;
    use std::fs;

    #[test]
    fn detects_conflict_markers_at_line_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conflict.txt");
        fs::write(
            &path,
            "<<<<<<< HEAD\nleft\n=======\nright\n>>>>>>> incoming\n",
        )
        .unwrap();
        assert!(conflict_markers_present(&path).unwrap());
    }

    #[test]
    fn ignores_missing_files_and_plain_text() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone.txt");
        assert!(!conflict_markers_present(&missing).unwrap());

        let plain = dir.path().join("plain.txt");
        fs::write(&plain, "no conflicts here\n").unwrap();
        assert!(!conflict_markers_present(&plain).unwrap());
    }

    #[test]
    fn hook_path_resolves_from_config_root_when_worktree_lacks_it() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        let linked = tmp.path().join("linked");
        fs::create_dir_all(main.join("scripts")).unwrap();
        fs::create_dir(&linked).unwrap();
        let hook = main.join("scripts/cw-restack-hook.sh");
        fs::write(&hook, "#!/bin/sh\n").unwrap();

        let mut cfg = Config::default();
        cfg.runtime.config_root = Some(main.clone());

        assert_eq!(hook_path(&cfg, &linked), Some(hook));
    }

    #[test]
    fn hook_path_prefers_local_worktree_over_config_root() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        let linked = tmp.path().join("linked");
        fs::create_dir_all(main.join("scripts")).unwrap();
        fs::create_dir_all(linked.join("scripts")).unwrap();
        fs::write(main.join("scripts/cw-restack-hook.sh"), "# main\n").unwrap();
        let local = linked.join("scripts/cw-restack-hook.sh");
        fs::write(&local, "# local\n").unwrap();

        let mut cfg = Config::default();
        cfg.runtime.config_root = Some(main);

        assert_eq!(hook_path(&cfg, &linked), Some(local));
    }
}

fn graphite_enabled(cfg: &Config) -> bool {
    cfg.integrations.graphite.unwrap_or_else(|| in_path("gt"))
}

fn in_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}
