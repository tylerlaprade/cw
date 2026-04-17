use super::schema::{Config, Runtime};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Load the effective config for `cwd`. Walks up to the git repo root,
/// reads `.devcli.toml` if present, then runs the autodetect pass.
pub fn load(cwd: &Path) -> Result<Config> {
    let repo_root = repo_root(cwd);
    let (path, mut cfg) = match &repo_root {
        Some(root) => {
            let p = root.join(".devcli.toml");
            if p.is_file() {
                let text = std::fs::read_to_string(&p)
                    .with_context(|| format!("reading {}", p.display()))?;
                let cfg: Config = toml::from_str(&text)
                    .with_context(|| format!("parsing {}", p.display()))?;
                (Some(p), cfg)
            } else {
                (None, Config::default())
            }
        }
        None => (None, Config::default()),
    };
    cfg.runtime = Runtime {
        repo_root: repo_root.clone(),
        config_path: path,
        stem: autodetect_stem(&cfg, repo_root.as_deref()),
        base_branch: autodetect_base_branch(&cfg, repo_root.as_deref()),
    };
    Ok(cfg)
}

fn repo_root(cwd: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

fn autodetect_stem(cfg: &Config, root: Option<&Path>) -> String {
    if let Some(s) = &cfg.workspace.stem {
        return s.clone();
    }
    // Prefer the main worktree's basename so we work from inside a worktree.
    let main = root.and_then(main_worktree).or_else(|| root.map(PathBuf::from));
    let base = main
        .as_deref()
        .and_then(|r| r.file_name())
        .map(|o| o.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workspace".into());
    // If the basename ends with _N (numeric), strip it — the stem is the
    // non-numbered form.
    strip_trailing_number_suffix(&base)
}

fn strip_trailing_number_suffix(name: &str) -> String {
    if let Some(idx) = name.rfind('_') {
        if name[idx + 1..].chars().all(|c| c.is_ascii_digit()) && idx + 1 < name.len() {
            return name[..idx].to_string();
        }
    }
    name.to_string()
}

fn main_worktree(inside: &Path) -> Option<PathBuf> {
    // `git worktree list --porcelain` starts with the main worktree.
    let out = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(inside)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8(out.stdout).ok()?;
    for line in stdout.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            return Some(PathBuf::from(p));
        }
    }
    None
}

fn autodetect_base_branch(cfg: &Config, root: Option<&Path>) -> String {
    if let Some(b) = &cfg.workspace.base_branch {
        return b.clone();
    }
    let Some(root) = root else {
        return "main".into();
    };
    for candidate in ["develop", "main", "master"] {
        let ok = Command::new("git")
            .args(["show-ref", "--verify", "--quiet"])
            .arg(format!("refs/heads/{}", candidate))
            .current_dir(root)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return candidate.into();
        }
    }
    "main".into()
}
