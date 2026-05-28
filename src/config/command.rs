use crate::cli::ConfigAction;
use anyhow::Result;
use owo_colors::OwoColorize;

pub fn run(action: ConfigAction) -> Result<()> {
    match action {
        ConfigAction::Show => show(),
        ConfigAction::Validate => validate(),
    }
}

fn show() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let cfg = super::discover::load(&cwd)?;
    let rt = &cfg.runtime;
    println!("# Effective cw config");
    println!("# repo_root   = {}", opt_path(rt.repo_root.as_deref()));
    println!("# config_path = {}", opt_path(rt.config_path.as_deref()));
    println!("# stem        = {}", rt.stem);
    println!("# base_branch = {}", rt.base_branch);
    println!();
    print!("{}", toml::to_string_pretty(&cfg)?);

    // J2: dep installers and env-copy are computed lazily at create time, not
    // stored on the config, so they wouldn't otherwise appear here. Surface the
    // autodetected ones (when not explicitly configured) so `config show` is the
    // honest "what will happen" view its docs promise.
    if let Some(root) = rt.repo_root.as_deref() {
        if cfg.deps.is_none() {
            let installs = crate::workspace::create::autodetect_dep_installs(root);
            if !installs.is_empty() {
                println!("\n# autodetected dep installs (run on create):");
                for i in &installs {
                    println!("#   {i}");
                }
            }
        }
        if cfg.env.copy.is_empty() {
            let envs = crate::workspace::create::autodetect_env_files(root);
            if !envs.is_empty() {
                println!("\n# autodetected env files (copied into each workspace):");
                println!("#   {}", envs.join(", "));
            }
        }
    }
    Ok(())
}

fn validate() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let cfg = super::discover::load(&cwd)?;
    let mut issues: Vec<String> = Vec::new();

    if let Some(r) = &cfg.restack.resolver {
        if !["claude", "codex", "manual"].contains(&r.as_str()) {
            issues.push(format!(
                "[restack] resolver {r:?} is not one of claude|codex|manual"
            ));
        }
    }
    if let Some(db) = &cfg.databases {
        if !["postgres", "none"].contains(&db.clone.as_str()) {
            issues.push(format!(
                "[databases] clone {:?} is not postgres|none",
                db.clone
            ));
        }
        if db.clone == "postgres" && !db.pattern.contains("{n}") {
            issues.push(format!(
                "[databases] pattern {:?} needs {{n}} to be per-workspace (else every workspace shares one DB)",
                db.pattern
            ));
        }
    }
    for rule in &cfg.env.strip {
        for p in &rule.patterns {
            if let Err(e) = regex::Regex::new(p) {
                issues.push(format!(
                    "[[env.strip]] file {:?}: invalid regex {p:?}: {e}",
                    rule.file
                ));
            }
        }
    }
    for s in &cfg.services {
        if s.start.is_none() {
            issues.push(format!("service {:?} has no `start` command", s.name));
        }
    }

    if issues.is_empty() {
        match &cfg.runtime.config_path {
            Some(p) => println!("ok: {}", p.display()),
            None => println!("ok: no .devcli.toml (all defaults)"),
        }
        Ok(())
    } else {
        for i in &issues {
            eprintln!("{} {}", "✗".red(), i);
        }
        anyhow::bail!("{} config issue(s)", issues.len())
    }
}

fn opt_path(p: Option<&std::path::Path>) -> String {
    p.map(|x| x.display().to_string())
        .unwrap_or_else(|| "<none>".into())
}
