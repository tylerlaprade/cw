use crate::cli::{RemoveArgs, WorkspaceAction, WorkspaceArgs};
use crate::config::{self, Config};
use crate::git::github;
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
    let numeric_head = head.parse::<u32>().ok();

    // Try to resolve the head as an existing target first (non-fatal on fail).
    let resolved = resolve::resolve(&cfg, &cwd, Some(&head)).ok();
    let create_from_pr = if resolved.is_none() {
        pr_target(&cfg, &head)?
    } else {
        None
    };
    if numeric_head.is_some() && resolved.is_none() && create_from_pr.is_none() {
        anyhow::bail!(
            "numeric target {head:?} did not match an existing workspace or PR; use spaces for new work"
        );
    }

    let flags = LaunchFlags {
        stack,
        continue_session: cont,
        pr_override: pr,
        prompt: if tail.is_empty() { None } else { Some(tail) },
    };

    if let Some(n) = numeric_head {
        let cap = cfg.workspace.max_count.unwrap_or(99);
        match (&resolved, &create_from_pr) {
            (Some(r), _) if r.pr.is_some() => {
                let pr = r.pr.unwrap();
                let branch = r.branch.as_deref().unwrap_or("");
                emitter.emit(Record::Msg(&format!("Found PR #{pr} → {branch}")));
                emitter.emit(Record::Msg(&format!(
                    "Branch already checked out in {}",
                    r.dir.display()
                )));
            }
            (Some(r), _) if n <= cap => {
                emitter.emit(Record::Msg(&format!(
                    "Switching to workspace {n} ({})",
                    r.dir.display()
                )));
            }
            (None, Some(pr_t)) => {
                emitter.emit(Record::Msg(&format!(
                    "Found PR #{} → {}",
                    pr_t.number, pr_t.branch
                )));
            }
            _ => {}
        }
    }

    match resolved {
        Some(r) => enter_workspace(&cfg, r, flags, emitter, false),
        None => {
            // No existing target — create a fresh workspace. For PR-create,
            // branch comes from the PR and prompt stays as tail only. For
            // description-create, the whole positional (head + tail) is both
            // the slug source and the Claude prompt — matching Bash `$*`.
            let is_description_create = create_from_pr.is_none();
            let full_input = positional.join(" ");
            let subject = if let Some(pr_target) = &create_from_pr {
                pr_target.branch.clone()
            } else {
                full_input.clone()
            };
            let _ = head;
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
            let launch_prompt = if is_description_create {
                Some(full_input)
            } else {
                flags.prompt.clone()
            };
            enter_workspace(
                &cfg,
                resolved,
                LaunchFlags {
                    pr_override: flags
                        .pr_override
                        .or(create_from_pr.as_ref().map(|target| target.number)),
                    prompt: launch_prompt,
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
    if let Some(n) = r.number {
        emitter.emit(Record::Title(&format!("#{}", n)));
    }
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
    cfg: &Config,
    r: resolve::Resolved,
    flags: LaunchFlags,
    emitter: &mut Emitter,
    first_entry: bool,
) -> Result<()> {
    emitter.emit(Record::Cd(&r.dir.to_string_lossy()));
    if let Some(n) = r.number {
        emitter.emit(Record::Title(&format!("#{}", n)));
    }
    if let Some(hook) = &cfg.hooks.post_cd {
        let argv = vec!["bash".into(), "-lc".into(), post_cd_command(&r, hook)];
        emitter.emit(Record::Exec(&argv));
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

    // Auto-launch Claude only on first entry, or when the user supplied a
    // prompt / explicit PR override / --continue. Bare `cw 8564` should just
    // enter the already-open workspace without trying to resume Claude.
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

struct PrTarget {
    number: u32,
    branch: String,
}

fn pr_target(cfg: &Config, token: &str) -> Result<Option<PrTarget>> {
    let Ok(number) = token.parse::<u32>() else {
        return Ok(None);
    };
    let Some(root) = cfg.runtime.repo_root.as_deref() else {
        return Ok(None);
    };
    let pr = match github::view_pr(root, number) {
        Ok(pr) => pr,
        Err(_) => return Ok(None),
    };
    Ok(Some(PrTarget {
        number,
        branch: pr.head_branch,
    }))
}

fn post_cd_command(r: &resolve::Resolved, hook: &str) -> String {
    let mut parts = vec![format!(
        "export DEVCLI_DIR={}",
        shell_quote(&r.dir.to_string_lossy())
    )];
    if let Some(branch) = &r.branch {
        parts.push(format!("export DEVCLI_BRANCH={}", shell_quote(branch)));
    }
    if let Some(number) = r.number {
        parts.push(format!("export DEVCLI_NUMBER={number}"));
    }
    parts.push(hook.to_string());
    parts.join("; ")
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
