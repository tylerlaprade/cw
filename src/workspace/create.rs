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
use std::path::{Path, PathBuf};
use std::process::Command;

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
    pub existed: bool,
    pub setup_log: PathBuf,
}

/// Build a branch name. If `subject` is already a plausible git ref
/// (contains only [A-Za-z0-9/_-]+ and is ≤ 100 chars), use it verbatim;
/// otherwise slugify.
pub fn branch_for_subject(subject: &str) -> String {
    let looks_like_ref = subject.len() <= 100
        && subject.chars().all(|c| {
            c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '/' || c == '.'
        });
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

    let number = claim_number(cfg, parent_dir)?;
    let dir = parent_dir.join(format!("{}_{}", cfg.runtime.stem, number));
    let existed = branch_exists(root, &branch)?;

    add_worktree(root, &dir, &branch, &parent_branch, existed)?;
    if !existed && graphite_enabled(cfg) {
        gt_track(&dir, &parent_branch)?;
    }

    copy_envs(root, &dir, cfg)?;
    strip_envs(&dir, cfg, number)?;
    inject_envs(&dir, cfg, number)?;

    let setup_log = PathBuf::from(format!(
        "/tmp/{}_{}_setup.log",
        cfg.runtime.stem, number
    ));
    let _ = std::fs::write(
        &setup_log,
        format!("# cw setup log for {} #{}\n", branch, number),
    );
    kick_off_setup(&dir, cfg, &setup_log)?;

    Ok(CreateResult {
        number,
        dir,
        branch,
        existed,
        setup_log,
    })
}

fn claim_number(cfg: &Config, parent: &Path) -> Result<u32> {
    let max = cfg.workspace.max_count.unwrap_or(99);
    let stem = &cfg.runtime.stem;
    for n in 1..=max {
        let candidate = parent.join(format!("{}_{}", stem, n));
        if candidate.exists() {
            continue;
        }
        let lock = PathBuf::from(format!("/tmp/.devcli_{}_{}_claim", stem, n));
        // Use mkdir-based lock for atomicity across processes.
        if std::fs::create_dir(&lock).is_ok() {
            // Double-check after claiming.
            if !candidate.exists() {
                return Ok(n);
            }
            let _ = std::fs::remove_dir(&lock);
        }
    }
    anyhow::bail!("no free workspace number ≤ {}", max);
}

fn branch_exists(inside: &Path, branch: &str) -> Result<bool> {
    let out = Command::new("git")
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/heads/{}", branch))
        .current_dir(inside)
        .status()?;
    if out.success() {
        return Ok(true);
    }
    let out = Command::new("git")
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/remotes/origin/{}", branch))
        .current_dir(inside)
        .status()?;
    Ok(out.success())
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
    let status = cmd
        .status()
        .with_context(|| format!("git worktree add {}", dir.display()))?;
    if !status.success() {
        anyhow::bail!("git worktree add failed");
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
            std::fs::create_dir_all(p).ok();
        }
        std::fs::copy(&from, &to)
            .with_context(|| format!("copy {} → {}", from.display(), to.display()))?;
    }
    Ok(())
}

fn autodetect_env_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for name in [".env", ".env.local"] {
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
    for rule in &cfg.env.inject {
        let path = dst.join(&rule.file);
        let line = rule
            .line
            .replace("{n}", &number.to_string())
            .replace("{stem}", &cfg.runtime.stem);
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

fn kick_off_setup(dir: &Path, cfg: &Config, log: &Path) -> Result<()> {
    let mut parts = Vec::new();
    if let Some(deps) = &cfg.deps {
        let joiner = if deps.parallel { " & " } else { " && " };
        let mut subparts = Vec::new();
        for i in &deps.install {
            subparts.push(format!("( cd {} && {} )", shell_quote(&i.dir), i.cmd));
        }
        if !subparts.is_empty() {
            let joined = subparts.join(joiner);
            let s = if deps.parallel {
                format!("{{ {joined}; wait; }}")
            } else {
                joined
            };
            parts.push(s);
        }
    } else {
        parts.extend(autodetect_dep_installs(dir));
    }

    if let Some(hook) = &cfg.hooks.post_create {
        parts.push(hook.clone());
    }

    if parts.is_empty() {
        // Nothing to do; mark done immediately.
        let _ = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(log)
            .and_then(|mut f| std::io::Write::write_all(&mut f, b"SETUP_DONE rc=0\n"));
        return Ok(());
    }

    let chain = parts.join(" && ");
    detach::spawn_shell_detached(&chain, dir, log, "SETUP_DONE")?;
    Ok(())
}

fn autodetect_dep_installs(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for entry in top_level_dirs(root) {
        let dirname = entry.file_name().unwrap().to_string_lossy().into_owned();
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

fn top_level_dirs(root: &Path) -> Vec<PathBuf> {
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
        let mut new_path = std::env::split_paths(&original_path.clone().unwrap_or_default())
            .collect::<Vec<_>>();
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
            runtime: Runtime {
                repo_root: Some(root.clone()),
                config_path: None,
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
        assert_eq!(
            fs::read_to_string(&gt_log).unwrap().trim(),
            "track --parent develop"
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
        let status = Command::new("git").args(args).current_dir(root).status().unwrap();
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
}
