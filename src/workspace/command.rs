use crate::cli::{RemoveArgs, WorkspaceAction, WorkspaceArgs};
use crate::config::{self, Config};
use crate::shell::{Emitter, Record};
use crate::workspace::{create, resolve};
use anyhow::{Context, Result};

/// Top-level dispatcher for bare `cw <args>`.
///
/// Parses a small flag set (`-s`/`--stack`, `--pr <N>`, `--continue`) and
/// a head positional. The head is tried as a resolvable target first
/// (number / PR# / branch); if nothing resolves, it's treated as a free-
/// form description for a new workspace.
pub fn default_dispatch(rest: Vec<String>, emitter: &mut Emitter) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let cfg = config::discover::load(&cwd)?;

    let Parsed {
        stack,
        pr,
        cont,
        positional,
    } = parse(rest)?;

    if positional.is_empty() {
        print_help();
        return Ok(());
    }

    let head = positional[0].clone();
    let tail = positional[1..].join(" ");

    // Try to resolve the head as an existing target first (non-fatal on fail).
    let resolved = resolve::resolve(&cfg, &cwd, Some(&head)).ok();

    let flags = LaunchFlags {
        stack,
        continue_session: cont,
        pr_override: pr,
        prompt: if tail.is_empty() { None } else { Some(tail) },
    };

    match resolved {
        Some(r) => enter_workspace(&cfg, r, flags, emitter, false),
        None => {
            // No existing target — create a fresh workspace with `head` as
            // description / branch name. If there was tail text, it becomes
            // the Claude prompt.
            let subject = if flags.prompt.is_some() {
                // head + tail was a full description; recombine.
                positional.join(" ")
            } else {
                head
            };
            let r = create::create(
                &cfg,
                &cwd,
                create::CreateOpts {
                    subject,
                    stack: flags.stack,
                    parent: None,
                },
            )?;
            emitter.emit(Record::Msg(&format!(
                "✓ workspace {} ready at {} (branch {})",
                r.number,
                r.dir.display(),
                r.branch
            )));
            let resolved = resolve::Resolved {
                dir: r.dir,
                number: Some(r.number),
                branch: Some(r.branch),
                pr: None,
            };
            enter_workspace(
                &cfg,
                resolved,
                LaunchFlags {
                    prompt: None,
                    ..flags
                },
                emitter,
                true,
            )
        }
    }
}

pub fn open(target: Option<String>, emitter: &mut Emitter) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let cfg = config::discover::load(&cwd)?;
    let r = resolve::resolve(&cfg, &cwd, target.as_deref())?;
    // CD into the workspace first.
    emitter.emit(Record::Cd(&r.dir.to_string_lossy()));
    // Then request the shell to invoke `cw serve start --open` in foreground.
    let argv = vec![
        "cw".into(),
        "serve".into(),
        "start".into(),
        "--open".into(),
    ];
    emitter.emit(Record::Exec(&argv));
    Ok(())
}

pub fn remove(_args: RemoveArgs, _emitter: &mut Emitter) -> Result<()> {
    Err(anyhow::anyhow!("`cw remove` lands in step 7"))
}

pub fn dispatch(args: WorkspaceArgs, emitter: &mut Emitter) -> Result<()> {
    match args.action {
        WorkspaceAction::List => Err(anyhow::anyhow!("`cw workspace list` lands in step 11")),
        WorkspaceAction::Resolve { target, json } => do_resolve(&target, json, emitter),
    }
}

// --- internals ------------------------------------------------------------

struct Parsed {
    stack: bool,
    pr: Option<u32>,
    cont: bool,
    positional: Vec<String>,
}

fn parse(args: Vec<String>) -> Result<Parsed> {
    let mut stack = false;
    let mut pr: Option<u32> = None;
    let mut cont = false;
    let mut positional = Vec::new();
    let mut iter = args.into_iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--stack" | "-s" => stack = true,
            "--continue" => cont = true,
            "--pr" => {
                let n = iter
                    .next()
                    .context("--pr requires a number")?
                    .parse()
                    .context("--pr expects a number")?;
                pr = Some(n);
            }
            "--" => positional.extend(iter.by_ref()),
            _ => positional.push(a),
        }
    }
    Ok(Parsed {
        stack,
        pr,
        cont,
        positional,
    })
}

#[derive(Debug, Clone)]
struct LaunchFlags {
    stack: bool,
    continue_session: bool,
    pr_override: Option<u32>,
    prompt: Option<String>,
}

fn enter_workspace(
    _cfg: &Config,
    r: resolve::Resolved,
    flags: LaunchFlags,
    emitter: &mut Emitter,
    first_entry: bool,
) -> Result<()> {
    emitter.emit(Record::Cd(&r.dir.to_string_lossy()));
    if let Some(n) = r.number {
        emitter.emit(Record::Title(&format!("#{}", n)));
    }

    // Decide what to launch (claude | codex | nothing).
    if let Some(argv) = compose_editor_launch(&r, &flags, first_entry) {
        emitter.emit(Record::Exec(&argv));
    }
    Ok(())
}

fn compose_editor_launch(
    r: &resolve::Resolved,
    flags: &LaunchFlags,
    first_entry: bool,
) -> Option<Vec<String>> {
    // Explicit --continue wins.
    if flags.continue_session {
        return Some(vec!["claude".into(), "--continue".into()]);
    }

    let has_prompt = flags.prompt.is_some();

    // Match the Bash dispatcher: only auto-launch Claude on first entry
    // (freshly-created workspace) or when the user supplied a prompt /
    // --pr / --continue. Bare `cw 3` just CDs.
    if !first_entry && !has_prompt && flags.pr_override.is_none() {
        return None;
    }

    let pr = flags.pr_override.or(r.pr);

    let mut argv = vec!["claude".into()];
    if let Some(n) = pr {
        argv.push("--from-pr".into());
        argv.push(n.to_string());
    }
    if let Some(branch) = &r.branch {
        argv.push("--name".into());
        argv.push(branch.clone());
    }
    if let Some(p) = &flags.prompt {
        argv.push(p.clone());
    }
    Some(argv)
}

fn do_resolve(target: &str, json: bool, _emitter: &mut Emitter) -> Result<()> {
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

fn print_help() {
    eprintln!("usage: cw <description|N|PR#|branch> [prompt...]");
    eprintln!("       cw -s <description>                  # stack on current branch");
    eprintln!("       cw <N> --continue                    # resume Claude session");
    eprintln!("       cw <N> --pr <N>                      # force PR association");
}
