//! `cw restack`: generic rebase loop + optional repo hook + resolver.

pub mod resolvers;

use crate::cli::RestackArgs;
use crate::config::{self, Config};
use crate::shell::Emitter;
use crate::workspace::resolve;
use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn run(args: RestackArgs, _emitter: &mut Emitter) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let cfg = config::discover::load(&cwd)?;
    let r = resolve::resolve(&cfg, &cwd, args.target.as_deref())?;
    let dir = r.dir.clone();

    let stashed = autostash(&dir)?;
    let out = run_loop(&cfg, &dir, &args);
    if stashed {
        restore_stash(&dir);
    }
    out
}

fn run_loop(cfg: &Config, dir: &Path, args: &RestackArgs) -> Result<()> {
    if !rebase_in_progress(dir) {
        // Kick off the rebase.
        if graphite_enabled(cfg) {
            let st = Command::new("gt")
                .args(["get", "--no-interactive"])
                .current_dir(dir)
                .status();
            if let Err(e) = st {
                eprintln!("warn: gt get failed: {e:#}");
            }
            println!("{} gt restack", "→".cyan());
            let st = Command::new("gt").arg("r").current_dir(dir).status()?;
            if st.success() && !rebase_in_progress(dir) {
                return finalize(cfg, dir);
            }
        } else {
            println!("{} git rebase {}", "→".cyan(), cfg.runtime.base_branch);
            let st = Command::new("git")
                .args(["rebase", &cfg.runtime.base_branch])
                .current_dir(dir)
                .status()?;
            if st.success() {
                return Ok(());
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

        let still = unresolved_files(dir)?;
        if !still.is_empty() {
            // 2. Fall through to the resolver.
            let resolver = pick_resolver(cfg, args);
            resolvers::run(resolver, dir, &still)?;
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
            // `gt continue` may have failed because the *next* commit conflicts.
            // Loop around — unresolved_files() will pick up the new set.
        }
    }
}

fn finalize(cfg: &Config, dir: &Path) -> Result<()> {
    if graphite_enabled(cfg) {
        // Optional: install deps after a restack — matches existing Condor behavior.
        // Kept light: no-op for now; users with heavy setups have post_create.
    }
    println!("{} restack complete", "✓".green());
    let _ = dir;
    Ok(())
}

// --- helpers --------------------------------------------------------------

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
    if let Some(configured) = &cfg.restack.hook {
        let p = dir.join(configured);
        if p.is_file() {
            return Some(p);
        }
        // Try resolving against repo root too.
        if let Some(root) = &cfg.runtime.repo_root {
            let p = root.join(configured);
            if p.is_file() {
                return Some(p);
            }
        }
        return None;
    }
    // Default locations.
    for candidate in ["scripts/cw-restack-hook.sh", ".cw/restack-hook.sh"] {
        let p = dir.join(candidate);
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
    let st = cmd.status().with_context(|| format!("running {}", hook.display()))?;
    if !st.success() {
        eprintln!(
            "{} hook exited {}; continuing to resolver",
            "⚠".yellow(),
            st.code().unwrap_or(-1)
        );
    }
    // Stage whatever the hook touched.
    let _ = Command::new("git").args(["add", "-u"]).current_dir(dir).status();
    Ok(())
}

fn pick_resolver(cfg: &Config, args: &RestackArgs) -> resolvers::Kind {
    if let Some(r) = args.resolver.as_deref() {
        return resolvers::Kind::parse(r);
    }
    if let Some(r) = cfg.restack.resolver.as_deref() {
        return resolvers::Kind::parse(r);
    }
    resolvers::Kind::autodetect()
}

fn graphite_enabled(cfg: &Config) -> bool {
    cfg.integrations.graphite.unwrap_or_else(|| in_path("gt"))
}

fn in_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}
