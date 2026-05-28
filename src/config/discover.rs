use super::schema::{Config, PortCfg, Runtime, ServiceCfg};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Load the effective config for `cwd`. Resolves the current worktree,
/// reads `.devcli.toml` from the worktree root — falling back to the main
/// worktree's copy so sibling worktrees inherit unless they override — then
/// runs the autodetect pass.
pub fn load(cwd: &Path) -> Result<Config> {
    let repo_root = repo_root(cwd);
    let (path, mut cfg) = match &repo_root {
        Some(root) => match find_config(root) {
            Some(p) => {
                let text = std::fs::read_to_string(&p)
                    .with_context(|| format!("reading {}", p.display()))?;
                let cfg: Config = toml::from_str(&text)
                    .with_context(|| format!("parsing {}", p.display()))?;
                (Some(p), cfg)
            }
            None => (None, Config::default()),
        },
        None => (None, Config::default()),
    };
    let config_root = path
        .as_ref()
        .and_then(|p| p.parent().map(PathBuf::from))
        .or_else(|| repo_root.as_deref().and_then(main_worktree))
        .or_else(|| repo_root.clone());
    cfg.runtime = Runtime {
        repo_root: repo_root.clone(),
        config_path: path,
        config_root,
        stem: autodetect_stem(&cfg, repo_root.as_deref()),
        base_branch: autodetect_base_branch(&cfg, repo_root.as_deref()),
    };
    // Merge autodetected services with any overrides already in cfg.services.
    autodetect_services(&mut cfg);
    Ok(cfg)
}

/// Fill `cfg.services` from repo layout when the file didn't specify them.
/// User-specified services are preserved verbatim; autodetection only adds
/// defaults for names not already present, and fills in missing fields on
/// existing entries by name match.
fn autodetect_services(cfg: &mut Config) {
    let Some(root) = cfg.runtime.repo_root.clone() else {
        return;
    };
    let detected = detect_services_in(&root);
    for d in detected {
        match cfg.services.iter_mut().find(|s| s.name == d.name) {
            Some(existing) => merge_service(existing, d),
            None => cfg.services.push(d),
        }
    }
}

fn merge_service(dst: &mut ServiceCfg, src: ServiceCfg) {
    if dst.subdir.is_none() {
        dst.subdir = src.subdir;
    }
    if dst.port.is_none() {
        dst.port = src.port;
    }
    if dst.start.is_none() {
        dst.start = src.start;
    }
    if dst.venv.is_none() {
        dst.venv = src.venv;
    }
    if dst.pid_file.is_none() {
        dst.pid_file = src.pid_file;
    }
    if dst.log_file.is_none() {
        dst.log_file = src.log_file;
    }
    if dst.stop_patterns.is_empty() {
        dst.stop_patterns = src.stop_patterns;
    }
    if dst.open_url.is_none() {
        dst.open_url = src.open_url;
    }
    if dst.alias.is_empty() {
        dst.alias = src.alias;
    }
}

fn detect_services_in(root: &Path) -> Vec<ServiceCfg> {
    let mut out = Vec::new();

    // Candidate dirs: the repo root itself (single-package layout — the common
    // non-monorepo case) FIRST, then each top-level subdir (monorepo layout).
    // Without the root candidate, a single-package repo autodetected to nothing.
    let candidates: Vec<(PathBuf, String)> = std::iter::once((root.to_path_buf(), ".".to_string()))
        .chain(top_level_dirs(root).into_iter().map(|d| {
            let name = d
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| ".".into());
            (d, name)
        }))
        .collect();

    let alias = |short: &str, subdir: &str| {
        let mut a = vec![short.to_string()];
        if subdir != "." {
            a.push(subdir.to_string());
        }
        a
    };

    // Backend: first candidate containing manage.py → Django.
    for (dir, subdir) in &candidates {
        if dir.join("manage.py").is_file() {
            out.push(ServiceCfg {
                name: "backend".into(),
                alias: alias("be", subdir),
                subdir: Some(subdir.clone()),
                port: Some(PortCfg { base: 8000 }),
                start: Some("python manage.py runserver {port}".into()),
                start_env: Default::default(),
                venv: Some(".venv/bin/activate".into()),
                pid_file: Some("/tmp/{stem}_{n}_backend.pid".into()),
                log_file: Some("/tmp/{stem}_{n}_backend.log".into()),
                // Port-scoped: each workspace's runserver port is unique.
                stop_patterns: vec!["manage.py runserver {port}".into()],
                pre_start: None,
                open_url: None,
            });
            break; // only one backend
        }
    }

    // Frontend: first candidate with package.json exposing a "dev"/"start" script.
    for (dir, subdir) in &candidates {
        if has_frontend_package(dir) {
            let start = if dir.join("vite.config.ts").is_file()
                || dir.join("vite.config.js").is_file()
            {
                "npm start -- --port {port}"
            } else {
                "npm run dev -- --port {port}"
            };
            // Workspace-scope the kill pattern with {stem}_{n}; otherwise
            // `cw serve stop` for one workspace matches (and kills) every other
            // workspace's frontend, since pkill -f is a substring match.
            let node_path = if subdir == "." {
                "{stem}_{n}/node_modules".to_string()
            } else {
                format!("{{stem}}_{{n}}/{subdir}/node_modules")
            };
            out.push(ServiceCfg {
                name: "frontend".into(),
                alias: alias("fe", subdir),
                subdir: Some(subdir.clone()),
                port: Some(PortCfg { base: 3000 }),
                start: Some(start.into()),
                start_env: Default::default(),
                venv: None,
                pid_file: Some("/tmp/{stem}_{n}_frontend.pid".into()),
                log_file: Some("/tmp/{stem}_{n}_frontend.log".into()),
                stop_patterns: vec![format!("{node_path}.*vite")],
                pre_start: None,
                open_url: Some("http://localhost:{port}".into()),
            });
            break; // only one frontend
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

fn has_frontend_package(dir: &Path) -> bool {
    let pkg = dir.join("package.json");
    if !pkg.is_file() {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(&pkg) else {
        return false;
    };
    // Very light parse: look for "scripts" + ("dev" or "start") keys.
    // Avoid pulling in serde_json just for this probe.
    let scripts_idx = text.find("\"scripts\"").unwrap_or(usize::MAX);
    if scripts_idx == usize::MAX {
        return false;
    }
    let tail = &text[scripts_idx..];
    tail.contains("\"dev\"") || tail.contains("\"start\"")
}

/// Look for `.devcli.toml` at `worktree_root`, then at the main worktree.
/// Worktree-local file wins, so a worktree can override the shared config.
fn find_config(worktree_root: &Path) -> Option<PathBuf> {
    let local = worktree_root.join(".devcli.toml");
    if local.is_file() {
        return Some(local);
    }
    let main = main_worktree(worktree_root)?;
    if main == worktree_root {
        return None;
    }
    let shared = main.join(".devcli.toml");
    shared.is_file().then_some(shared)
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
