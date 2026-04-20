use crate::cli::{RemoveArgs, WorkspaceAction, WorkspaceArgs};
use crate::config;
use crate::shell::Emitter;
use crate::workspace::{create, resolve};
use anyhow::{Context, Result};

pub fn default_dispatch(_rest: Vec<String>, _emitter: &mut Emitter) -> Result<()> {
    Err(anyhow::anyhow!(
        "bare `cw <description|N|PR#|branch>` lands in steps 4-5"
    ))
}

pub fn open(_target: Option<String>, _emitter: &mut Emitter) -> Result<()> {
    Err(anyhow::anyhow!("`cw open` lands in step 5"))
}

pub fn remove(_args: RemoveArgs, _emitter: &mut Emitter) -> Result<()> {
    Err(anyhow::anyhow!("`cw remove` lands in step 7"))
}

pub fn dispatch(args: WorkspaceArgs, _emitter: &mut Emitter) -> Result<()> {
    match args.action {
        WorkspaceAction::List => Err(anyhow::anyhow!("`cw workspace list` lands in step 11")),
        WorkspaceAction::Resolve { target, json } => do_resolve(&target, json),
        WorkspaceAction::NextNumber => do_next_number(),
    }
}

fn do_next_number() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let cfg = config::discover::load(&cwd)?;
    let root = cfg
        .runtime
        .repo_root
        .as_deref()
        .context("not inside a git repo")?;
    let parent = root.parent().context("repo root has no parent")?;
    let (n, lock) = create::claim_number(&cfg, parent, std::path::Path::new("/tmp"))?;
    lock.release();
    println!("{}", n);
    Ok(())
}

fn do_resolve(target: &str, json: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let cfg = config::discover::load(&cwd)?;
    let r = resolve::resolve(&cfg, &cwd, Some(target))?;
    if json {
        print_json(&r);
    } else {
        print_plain(&r);
    }
    Ok(())
}

fn print_plain(r: &resolve::Resolved) {
    println!("dir     {}", r.dir.display());
    if let Some(n) = r.number {
        println!("number  {n}");
    }
    if let Some(b) = &r.branch {
        println!("branch  {b}");
    }
    if let Some(p) = r.pr {
        println!("pr      {p}");
    }
}

fn print_json(r: &resolve::Resolved) {
    let mut parts = Vec::new();
    parts.push(format!("\"dir\":{:?}", r.dir.display().to_string()));
    if let Some(n) = r.number {
        parts.push(format!("\"number\":{}", n));
    }
    if let Some(b) = &r.branch {
        parts.push(format!("\"branch\":{:?}", b));
    }
    if let Some(p) = r.pr {
        parts.push(format!("\"pr\":{}", p));
    }
    println!("{{{}}}", parts.join(","));
}
