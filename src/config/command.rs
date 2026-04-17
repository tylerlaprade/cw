use crate::cli::ConfigAction;
use anyhow::Result;

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
    Ok(())
}

fn validate() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let cfg = super::discover::load(&cwd)?;
    match &cfg.runtime.config_path {
        Some(p) => println!("ok: {}", p.display()),
        None => println!("ok: no .devcli.toml (all defaults)"),
    }
    Ok(())
}

fn opt_path(p: Option<&std::path::Path>) -> String {
    p.map(|x| x.display().to_string()).unwrap_or_else(|| "<none>".into())
}
