//! `cw cleanup`: sweep stale workspaces + branches + orphaned DBs.

use crate::cli::CleanupArgs;
use crate::config::{self, Config};
use crate::shell::Emitter;
use crate::workspace::{inventory, teardown};
use anyhow::Result;
use owo_colors::OwoColorize;
use std::process::Command;

pub fn run(args: CleanupArgs, emitter: &mut Emitter) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let cfg = config::discover::load(&cwd)?;
    let Some(root) = cfg.runtime.repo_root.clone() else {
        anyhow::bail!("not inside a git repo");
    };

    if !args.dry_run {
        println!("{} git fetch --prune origin", "→".cyan());
        let _ = Command::new("git")
            .args(["fetch", "--prune", "origin"])
            .current_dir(&root)
            .status();
    }

    // Inventory stale workspaces.
    let entries = inventory::list_workspaces(&cfg)?;
    let stale: Vec<_> = entries
        .into_iter()
        .filter(|e| {
            // Skip the main worktree — never cleans itself.
            e.dir != root
                && e.number.is_some()
                && e.is_removable()
        })
        .collect();

    if stale.is_empty() {
        println!("{} no stale workspaces", "✓".green());
    } else {
        println!(
            "{} {} stale workspace(s):",
            "·".dimmed(),
            stale.len().bold()
        );
        for e in &stale {
            let reasons = collect_reasons(e);
            println!(
                "  #{} {} — {}",
                e.number.unwrap(),
                e.dir.display(),
                reasons.join(", ")
            );
        }
    }

    if !args.dry_run && !stale.is_empty() {
        let targets: Vec<String> = stale
            .iter()
            .map(|e| e.number.unwrap().to_string())
            .collect();
        teardown::run(
            &cfg,
            &targets,
            &teardown::RemoveOpts {
                force: args.force,
                dry_run: false,
                no_close_tab: false,
            },
            emitter,
        )?;
    }

    // Orphaned DBs (pattern exists but no {stem}_{N} dir).
    if let Some(db) = &cfg.databases {
        warn_orphaned_dbs(&cfg, db);
    }

    // Graphite sync at the end.
    if graphite_enabled(&cfg) && !args.dry_run {
        println!("{} gt sync", "→".cyan());
        let _ = Command::new("gt").arg("sync").current_dir(&root).status();
    }
    Ok(())
}

fn collect_reasons(e: &inventory::Entry) -> Vec<String> {
    let mut r = Vec::new();
    if e.detached {
        r.push("detached".to_string());
    }
    if e.remote_gone {
        r.push("remote gone".to_string());
    }
    if e.merged {
        r.push("merged into base".to_string());
    }
    if e.no_unique_commits {
        r.push("no unique commits".to_string());
    }
    if let Some(pr) = e.pr_closed_or_merged {
        r.push(format!("PR #{pr} closed/merged"));
    }
    r
}

fn warn_orphaned_dbs(cfg: &Config, db: &crate::config::schema::DatabasesCfg) {
    // Query all matching DBs once, then cross-reference against known workspace
    // numbers by parsing the pattern.
    let Some(stem) = Some(cfg.runtime.stem.clone()) else {
        return;
    };
    // e.g. pattern "hanaq_{n}_{suffix}" with suffixes [qa, stg] → regex
    // ^hanaq_(\d+)_(qa|stg)$
    let suffix_group = db.suffixes.join("|");
    let pat = db
        .pattern
        .replace("{n}", r"(\d+)")
        .replace("{suffix}", &format!("({})", suffix_group));
    let Ok(re) = regex::Regex::new(&format!("^{}$", pat)) else {
        return;
    };

    let out = Command::new("psql")
        .args(["-At", "-c", "SELECT datname FROM pg_database"])
        .output();
    let Ok(out) = out else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let Some(parent) = cfg.runtime.repo_root.as_deref().and_then(|r| r.parent()) else {
        return;
    };
    let mut warned = 0;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some(caps) = re.captures(line) else {
            continue;
        };
        let n: u32 = caps.get(1).unwrap().as_str().parse().unwrap_or(0);
        let dir = parent.join(format!("{}_{}", stem, n));
        if !dir.is_dir() {
            println!(
                "{} orphaned DB: {} (no matching workspace dir)",
                "⚠".yellow(),
                line
            );
            warned += 1;
        }
    }
    if warned > 0 {
        println!(
            "  (drop manually or pass --force on `cw remove <N>` after recreating the dir)"
        );
    }
}

fn graphite_enabled(cfg: &Config) -> bool {
    cfg.integrations.graphite.unwrap_or_else(|| {
        std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).any(|d| d.join("gt").is_file()))
            .unwrap_or(false)
    })
}
