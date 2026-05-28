//! `cw init`: scaffold a `.devcli.toml` in the repo root.
//!
//! Interactive prompts collect the overrides the user wants right now, then
//! the file is written with those overrides uncommented at the top and a full
//! commented scaffold of every other knob below. That scaffold is what lets
//! future-you flip any decision later without digging through source.
//!
//! Idempotent — refuses to clobber an existing file unless `CW_INIT_FORCE=1`.

use anyhow::{Context, Result};
use inquire::{Confirm, Text};
use owo_colors::OwoColorize;

pub fn run() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let cfg = super::discover::load(&cwd)?;
    let root = cfg
        .runtime
        .repo_root
        .clone()
        .context("not inside a git repo")?;
    let out = root.join(".devcli.toml");

    if out.exists() && std::env::var_os("CW_INIT_FORCE").is_none() {
        eprintln!(
            "{} {} already exists. Set CW_INIT_FORCE=1 to overwrite.",
            "✗".red(),
            out.display()
        );
        return Err(anyhow::anyhow!("refusing to overwrite"));
    }

    println!("{} autodetected:", "·".dimmed());
    println!("  stem        {}", cfg.runtime.stem);
    println!("  base_branch {}", cfg.runtime.base_branch);
    println!("  services    {}", cfg.services.len());

    let is_tty = std::io::IsTerminal::is_terminal(&std::io::stdin());

    // Overrides the user actively opts into go here, uncommented, at the top
    // of the generated file. Everything else lives in the commented scaffold.
    let mut overrides: Vec<String> = Vec::new();

    let max_count = if is_tty {
        // J6: propagate Ctrl-C via `?` (don't silently treat an interrupt as
        // "unlimited"); surface a note when the input is non-numeric.
        let raw = Text::new("Maximum workspace count (leave blank for unlimited):")
            .with_default("")
            .prompt()?;
        let raw = raw.trim();
        if raw.is_empty() {
            None
        } else {
            match raw.parse::<u32>() {
                Ok(n) => Some(n),
                Err(_) => {
                    eprintln!(
                        "  {} ignoring non-numeric max_count {raw:?} (leaving unlimited)",
                        "·".dimmed()
                    );
                    None
                }
            }
        }
    } else {
        None
    };
    if let Some(n) = max_count {
        overrides.push("[workspace]".into());
        overrides.push(format!("max_count = {n}"));
        overrides.push(String::new());
    }

    let want_db = if is_tty {
        Confirm::new("Configure per-workspace databases?")
            .with_default(false)
            .prompt()?
    } else {
        false
    };
    if want_db {
        let pattern = Text::new("DB name pattern (use {n} and {suffix}):")
            .with_default("app_{n}_{suffix}")
            .prompt()?;
        let suffixes = Text::new("Comma-separated suffixes:")
            .with_default("qa")
            .prompt()?;
        overrides.push("[databases]".into());
        // J5: escape user input so a stray quote/backslash can't produce an
        // unparseable .devcli.toml.
        overrides.push(format!("pattern  = {}", toml_str(&pattern)));
        let s_list = suffixes
            .split(',')
            .map(|s| toml_str(s.trim()))
            .collect::<Vec<_>>()
            .join(", ");
        overrides.push(format!("suffixes = [{}]", s_list));
        overrides.push("clone    = \"postgres\"".into());
        overrides.push(String::new());
    }

    let want_hook = if is_tty {
        Confirm::new("Add a restack hook stub script?")
            .with_default(false)
            .prompt()?
    } else {
        false
    };
    if want_hook {
        let hook_path = "scripts/cw-restack-hook.sh";
        let hook_full = root.join(hook_path);
        if !hook_full.exists() {
            if let Some(p) = hook_full.parent() {
                std::fs::create_dir_all(p).ok();
            }
            std::fs::write(
                &hook_full,
                "#!/usr/bin/env bash\n\
                 # cw restack hook. Invoked when rebase conflicts appear.\n\
                 # Args: list of unresolved paths.\n\
                 # Contract: do whatever (makemigrations merge, pytest --snapshot-update,\n\
                 # lint --fix), stage what you resolve, exit 0. Anything left unresolved\n\
                 # falls through to the configured resolver (claude / codex / manual).\n\
                 set -euo pipefail\n\
                 echo \"hook: $# unresolved file(s)\"\n\
                 for f in \"$@\"; do echo \"  $f\"; done\n",
            )?;
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&hook_full)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&hook_full, perms)?;
            println!("{} wrote {}", "✓".green(), hook_full.display());
        }
        overrides.push("[restack]".into());
        overrides.push(format!("hook = \"{hook_path}\""));
        overrides.push(String::new());
    }

    let mut text = String::new();
    text.push_str(HEADER);
    text.push('\n');
    if !overrides.is_empty() {
        text.push_str(
            "# --- active overrides ---------------------------------------------------\n\n",
        );
        text.push_str(&overrides.join("\n"));
        text.push('\n');
    }
    text.push_str(SCAFFOLD);

    std::fs::write(&out, &text).with_context(|| format!("writing {}", out.display()))?;
    println!("{} wrote {}", "✓".green(), out.display());
    println!(
        "  Everything beyond the active overrides is commented — flip on later\n  \
         by uncommenting the line you want. Re-run `cw config show` anytime to\n  \
         see the effective merged config."
    );
    Ok(())
}

/// Quote + escape a string as a TOML basic string, so user input containing a
/// quote or backslash can't produce an unparseable `.devcli.toml`.
fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::toml_str;

    #[test]
    fn toml_str_escapes_quotes_and_backslashes() {
        assert_eq!(toml_str("app_{n}_{suffix}"), "\"app_{n}_{suffix}\"");
        assert_eq!(toml_str(r#"a"b"#), r#""a\"b""#);
        assert_eq!(toml_str(r"a\b"), r#""a\\b""#);
        // The escaped output must round-trip through the TOML parser.
        let v: toml::Value = toml::from_str(&format!("k = {}", toml_str(r#"weird"\val"#))).unwrap();
        assert_eq!(v["k"].as_str().unwrap(), r#"weird"\val"#);
    }
}

const HEADER: &str = "\
# .devcli.toml — cw workspace tooling config.
#
# cw runs with zero config: it autodetects repo root, stem, base branch, and
# any Django (manage.py) / Node (package.json with a \"dev\" or \"start\" script)
# services. This file only exists so future-you can override those defaults
# without digging through source.
#
# Placeholders in string values:
#   {n}     workspace number
#   {stem}  repo stem (basename of main worktree, minus any _N suffix)
#   {port}  computed port (port.base + n)
#
# Inspect the effective merged config any time:    cw config show
# Validate this file after editing:                cw config validate
";

const SCAFFOLD: &str = "
# ---------------------------------------------------------------------------
# [workspace] — numbering + branch basics.
# ---------------------------------------------------------------------------
# [workspace]
# max_count   = 48           # cap on workspace count. Default: unlimited.
# base_branch = \"develop\"    # trunk branch. Default: develop | main | master.
# stem        = \"myproject\"  # workspace dir prefix. Default: repo-root basename
#                            # with any trailing _N stripped.

# ---------------------------------------------------------------------------
# [integrations] — opt in/out of external tool integrations. Omit a key to let
# cw autodetect from $PATH. Set `false` to force-disable even if installed.
# ---------------------------------------------------------------------------
# [integrations]
# graphite = true   # gt stacking
# github   = true   # gh PR resolution + triage
# claude   = true   # restack resolver + workspace launch
# codex    = true   # alternate restack resolver
# direnv   = true   # auto-allow .envrc on new workspaces
# acli     = true   # Jira triage

# ---------------------------------------------------------------------------
# [[services]] — repeatable. cw autodetects Django + Node services; only add
# entries here to override autodetection or declare something it can't guess.
# ---------------------------------------------------------------------------
# [[services]]
# name          = \"backend\"
# alias         = [\"be\", \"api\"]
# subdir        = \"server\"
# port.base     = 8000
# start         = \"python manage.py runserver {port}\"
# venv          = \".venv/bin/activate\"
# pid_file      = \"/tmp/{stem}_{n}_backend.pid\"
# log_file      = \"/tmp/{stem}_{n}_backend.log\"
# stop_patterns = [\"manage.py runserver {port}\"]
# pre_start     = \"python manage.py migrate --noinput\"
# open_url      = \"http://localhost:{port}\"
# start_env     = { DJANGO_SETTINGS_MODULE = \"project.settings.dev\" }

# [[services]]
# name      = \"frontend\"
# alias     = [\"fe\"]
# subdir    = \"web\"
# port.base = 3000
# start     = \"npm run dev -- --port {port}\"
# open_url  = \"http://localhost:{port}\"

# ---------------------------------------------------------------------------
# [deps] — dependency install commands run on workspace creation.
# ---------------------------------------------------------------------------
# [deps]
# parallel = true
# install  = [
#   { dir = \"server\", cmd = \"uv sync\" },
#   { dir = \"web\",   cmd = \"npm ci\"  },
# ]

# ---------------------------------------------------------------------------
# [databases] — per-workspace DB clone. `pattern` names the per-workspace DB;
# `suffixes` lists the envs to clone. Source = `{pattern}` filled with
# `default_source_suffix`.
# ---------------------------------------------------------------------------
# [databases]
# pattern               = \"myapp_{n}_{suffix}\"
# suffixes              = [\"qa\", \"stg\", \"prod\"]
# clone                 = \"postgres\"   # or \"none\"
# default_source_suffix = \"qa\"

# ---------------------------------------------------------------------------
# [restack] — hook script + conflict resolver.
# ---------------------------------------------------------------------------
# [restack]
# hook     = \"./scripts/cw-restack-hook.sh\"  # runs after rebase, before commit.
# resolver = \"claude\"                        # claude | codex | manual

# ---------------------------------------------------------------------------
# [hooks] — lifecycle hooks. Each value is a shell snippet.
# Env exposed to post_cd: DEVCLI_DIR, DEVCLI_BRANCH, DEVCLI_NUMBER.
# ---------------------------------------------------------------------------
# [hooks]
# post_create = \"./scripts/cw-post-create.sh\"
# pre_remove  = \"./scripts/cw-pre-remove.sh\"
# post_cd     = \"source .venv/bin/activate 2>/dev/null || true\"

# ---------------------------------------------------------------------------
# [env] — copy + mutate env files into each new workspace.
# ---------------------------------------------------------------------------
# [env]
# copy = [\".env\", \"server/.env.local\"]
#
# [[env.strip]]
# file     = \".env\"
# patterns = [\"^DATABASE_URL=\", \"^REDIS_URL=\"]
#
# [[env.inject]]
# file = \".env\"
# line = \"DATABASE_URL=postgres:///{stem}_{n}_qa\"
#
# [[env.inject]]
# file = \".env\"
# line = \"PORT={port}\"
";
