use crate::cli::{RemoveArgs, ServeArgs, WorkspaceAction, WorkspaceArgs};
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
        base,
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
    let full_input = positional.join(" ");
    let prospective_branch = create::branch_for_subject(&full_input);
    // A single verbatim token naming an existing branch is a checkout request,
    // even if that branch is not currently checked out in any worktree.
    let existing_branch_entry = create_from_pr.is_none()
        && positional.len() == 1
        && prospective_branch == full_input
        && cfg
            .runtime
            .repo_root
            .as_deref()
            .map(|root| create::branch_exists(root, &prospective_branch).unwrap_or(false))
            .unwrap_or(false);

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
            // C2: a single branch-like token (contains -, _, or /) that
            // resolved to nothing is almost always a typo'd branch name, not a
            // request to create a phantom branch off base. Refuse — multi-word
            // descriptions and `--stack` slugs are still allowed.
            if !flags.stack
                && create_from_pr.is_none()
                && !existing_branch_entry
                && positional.len() == 1
                && head.contains(['-', '_', '/'])
            {
                anyhow::bail!(
                    "{head:?} looks like a branch name but doesn't exist locally or on origin.\n  \
                     To start new work with this name, use spaces: cw <description>"
                );
            }

            // No existing target — create a fresh workspace. For PR-create,
            // branch comes from the PR and prompt stays as tail only. For
            // description-create, the whole positional (head + tail) is both
            // the slug source and the Claude prompt — matching Bash `$*`.
            let is_description_create = create_from_pr.is_none() && !existing_branch_entry;
            let subject = if let Some(pr_target) = &create_from_pr {
                pr_target.branch.clone()
            } else if existing_branch_entry {
                prospective_branch.clone()
            } else {
                full_input.clone()
            };
            let _ = head;

            // F2: if a branch in the same stack as the target is already checked
            // out in a sibling worktree, enter it instead of creating a
            // duplicate worktree for the stack. (For a brand-new description the
            // slug branch doesn't exist yet, so this is a no-op there.)
            if let Some(root) = cfg.runtime.repo_root.as_deref() {
                if let Some(hit) = crate::git::graphite::find_stack_worktree(
                    root,
                    &prospective_branch,
                    &cfg.runtime.base_branch,
                ) {
                    if let Ok(r) = resolve::resolve(&cfg, &cwd, Some(&hit.branch)) {
                        emitter.emit(Record::Msg(&format!(
                            "Stack overlap: {} already in {} (same stack as {})",
                            hit.branch,
                            r.dir.display(),
                            prospective_branch
                        )));
                        let launch_prompt = if existing_branch_entry {
                            None
                        } else if is_description_create {
                            Some(full_input)
                        } else {
                            flags.prompt.clone()
                        };
                        let pr_override = flags
                            .pr_override
                            .or(create_from_pr.as_ref().map(|t| t.number));
                        return enter_workspace(
                            &cfg,
                            r,
                            LaunchFlags {
                                pr_override,
                                prompt: launch_prompt,
                                ..flags
                            },
                            emitter,
                            false,
                        );
                    }
                }
            }

            let launch_prompt = if existing_branch_entry {
                None
            } else if is_description_create {
                Some(full_input)
            } else {
                flags.prompt.clone()
            };

            let created = create::create(
                &cfg,
                &cwd,
                create::CreateOpts {
                    subject,
                    stack: flags.stack,
                    parent: base,
                },
            );
            let r = match created {
                Ok(r) => r,
                Err(e) => {
                    // C4: the branch is already checked out in another worktree
                    // (e.g. mid-rebase, so the stack pre-empt above missed it via
                    // stale ancestry). Switch into that worktree with --continue
                    // instead of failing with a raw `git worktree add` error —
                    // mirrors the original's rc-2 → --continue recovery.
                    let err_str = format!("{e:#}");
                    if is_busy_worktree_error(&err_str) {
                        // Prefer the exact path git named — it works even when
                        // that worktree is DETACHED mid-rebase, where a
                        // branch→worktree lookup (resolve) finds nothing (HEAD is
                        // detached, so porcelain shows no `branch refs/heads/…`).
                        // Mirrors the original's stderr-path extraction.
                        let busy = parse_busy_worktree_path(&err_str)
                            .map(|dir| resolve::Resolved {
                                number: crate::util::paths::detect_number(&dir, &cfg.runtime.stem),
                                pr: github::pr_for_branch(&dir, &prospective_branch),
                                branch: Some(prospective_branch.clone()),
                                dir,
                            })
                            .or_else(|| {
                                resolve::resolve(&cfg, &cwd, Some(&prospective_branch)).ok()
                            });
                        if let Some(r) = busy {
                            emitter.emit(Record::Msg(&format!(
                                "Branch {prospective_branch} already in use by {} (may be mid-rebase) — switching there",
                                r.dir.display()
                            )));
                            let pr_override = flags
                                .pr_override
                                .or(create_from_pr.as_ref().map(|t| t.number));
                            return enter_workspace(
                                &cfg,
                                r,
                                LaunchFlags {
                                    continue_session: true,
                                    pr_override,
                                    prompt: launch_prompt,
                                    ..flags
                                },
                                emitter,
                                false,
                            );
                        }
                    }
                    return Err(e);
                }
            };

            // F3: entering an EXISTING branch that wasn't checked out anywhere
            // (so we just created a fresh worktree for it) — resume its linked
            // PR's Claude session, matching the original `gh pr list --head`
            // lookup. New description-slugs have no PR yet, so this is a no-op there.
            let resumed_pr = if existing_branch_entry {
                cfg.runtime
                    .repo_root
                    .as_deref()
                    .and_then(|root| github::pr_for_branch(root, &r.branch))
            } else {
                None
            };

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
                    pr_override: flags
                        .pr_override
                        .or(create_from_pr.as_ref().map(|target| target.number))
                        .or(resumed_pr),
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
        if n != 0 {
            emitter.emit(Record::Title(&format!("#{}", n)));
        }
    }
    if emitter.enabled() {
        // Under the wrapper: it cd's into the workspace, then runs this.
        let argv = vec!["cw".into(), "serve".into(), "start".into(), "--open".into()];
        emitter.emit(Record::Exec(&argv));
    } else {
        // G6: no wrapper, so the CD record above does nothing and the EXEC
        // record would be printed and ignored — `cw open` was a silent no-op.
        // Start services + open the browser directly for the resolved workspace.
        eprintln!(
            "note: not running under the cw shell wrapper — starting services in place (no cd)"
        );
        let serve_target = r.number.map(|n| n.to_string()).or(target);
        crate::serve::run(
            ServeArgs {
                action: "start".into(),
                target: serve_target,
                tail: false,
                open: true,
                no_ai: false,
                lines: None,
                service: None,
            },
            emitter,
        )?;
    }
    Ok(())
}

pub fn remove(args: RemoveArgs, emitter: &mut Emitter) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let cfg = config::discover::load(&cwd)?;
    let opts = crate::workspace::teardown::RemoveOpts {
        force: args.force,
        dry_run: args.dry_run,
        no_close_tab: args.no_close_tab,
        stale_hours: None,
    };
    crate::workspace::teardown::run(&cfg, &args.targets, &opts, emitter)
}

pub fn dispatch(args: WorkspaceArgs, emitter: &mut Emitter) -> Result<()> {
    match args.action {
        WorkspaceAction::List => do_list(),
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
    // Per-repo claim lock dir (the repo's git dir), not global /tmp.
    let lock_dir = create::claim_lock_dir(root);
    let (n, lock) = create::claim_number(&cfg, parent, &lock_dir)?;
    lock.release();
    println!("{}", n);
    Ok(())
}

fn do_list() -> Result<()> {
    use owo_colors::OwoColorize;
    let cwd = std::env::current_dir()?;
    let cfg = config::discover::load(&cwd)?;
    let entries = crate::workspace::inventory::list_workspaces(&cfg)?;
    println!(
        "{:>3}  {:<30}  {:<22}  {}",
        "N".bold(),
        "branch".bold(),
        "dir".bold(),
        "flags".bold()
    );
    for e in &entries {
        let n = e
            .number
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".into());
        let branch = e.branch.as_deref().unwrap_or("(detached)");
        let dir = e
            .dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut flags = Vec::new();
        if e.merged {
            flags.push("merged".yellow().to_string());
        }
        if e.remote_gone {
            flags.push("remote-gone".yellow().to_string());
        }
        if e.detached {
            flags.push("detached".dimmed().to_string());
        }
        if e.no_unique_commits {
            flags.push("no-unique".dimmed().to_string());
        }
        if let Some(pr) = e.pr_closed_or_merged {
            flags.push(format!("pr#{pr} gone").yellow().to_string());
        }
        println!(
            "{:>3}  {:<30}  {:<22}  {}",
            n,
            truncate(branch, 30),
            dir,
            flags.join(", ")
        );
    }
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let head: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

// --- internals ------------------------------------------------------------

struct Parsed {
    stack: bool,
    pr: Option<u32>,
    cont: bool,
    base: Option<String>,
    positional: Vec<String>,
}

fn parse(args: Vec<String>) -> Result<Parsed> {
    let mut stack = false;
    let mut pr: Option<u32> = None;
    let mut cont = false;
    let mut base: Option<String> = None;
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
            // Branch a new workspace off an arbitrary base (a release branch, a
            // teammate's branch, a swarm foundation) instead of the autodetected
            // trunk. Mirrors new-workspace.sh's `--base`.
            "--base" => {
                base = Some(iter.next().context("--base requires a branch name")?);
            }
            "--" => positional.extend(iter.by_ref()),
            _ => positional.push(a),
        }
    }
    Ok(Parsed {
        stack,
        pr,
        cont,
        base,
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
    // K2: allow direnv's .envrc on FIRST entry, before cd'ing in — otherwise
    // entering the new worktree trips direnv's "blocked" prompt. Foreground
    // EXEC emitted ahead of the CD record so it runs first.
    if first_entry {
        let direnv = cfg
            .integrations
            .direnv
            .unwrap_or_else(|| crate::util::in_path("direnv"));
        if direnv {
            // Allow the root .envrc AND any top-level subdir .envrc (a monorepo
            // subproject often ships its own). The original allowed a nested
            // `hanaq/.envrc`; generalized here to every top-level subdir so we
            // don't trip direnv's "blocked" prompt on first entry.
            let mut envrcs = vec![r.dir.join(".envrc")];
            for sub in create::top_level_dirs(&r.dir) {
                envrcs.push(sub.join(".envrc"));
            }
            for envrc in envrcs {
                if envrc.is_file() {
                    emitter.emit(Record::Exec(&[
                        "direnv".into(),
                        "allow".into(),
                        envrc.to_string_lossy().into_owned(),
                    ]));
                }
            }
        }
    }

    emitter.emit(Record::Cd(&r.dir.to_string_lossy()));
    if let Some(n) = r.number {
        if n != 0 {
            emitter.emit(Record::Title(&format!("#{}", n)));
        }
    }

    // C5: on re-entry, warn if this workspace's background setup hasn't
    // finished (deps/DB/hooks) — the create path prints this on first entry.
    if !first_entry {
        if let Some(n) = r.number.filter(|n| *n != 0) {
            let log = format!("/tmp/{}_{}_setup.log", cfg.runtime.stem, n);
            if std::fs::read_to_string(&log)
                .map(|c| !c.contains("SETUP_DONE"))
                .unwrap_or(false)
            {
                emitter.emit(Record::Msg(&format!(
                    "⚠ Background setup still running. Tail: tail -f {log}"
                )));
            }
        }

        // K2: background-restack the workspace onto base on re-entry, like the
        // original bg_restack — non-interactive and ABORT-ON-CONFLICT, so it
        // never leaves the worktree mid-rebase (a manual `cw restack` resolves).
        // Opt-in: it rewrites local history in the background, which surprises
        // (force-push needed after). Enable with `[workspace] auto_restack = true`.
        if cfg.workspace.auto_restack {
            let graphite = cfg
                .integrations
                .graphite
                .unwrap_or_else(|| crate::util::in_path("gt"));
            let inner = if graphite {
                // No `--force`: re-entry restack runs on an actively-worked
                // branch that likely has un-pushed local commits; forcing would
                // reset them to origin (T1.2 — the original only forced on a
                // freshly-created `--first` workspace, never on normal re-entry).
                "gt get </dev/null >/dev/null 2>&1 && gt r --quiet </dev/null 2>&1 \
                 || git rebase --abort >/dev/null 2>&1 || true"
                    .to_string()
            } else {
                format!(
                    "git fetch origin >/dev/null 2>&1; \
                     git rebase origin/{base} </dev/null >/dev/null 2>&1 \
                     || git rebase --abort >/dev/null 2>&1 || true",
                    base = cfg.runtime.base_branch
                )
            };
            let cmd = format!(
                "cd {} && {{ {inner}; }}",
                shell_quote(&r.dir.to_string_lossy())
            );
            emitter.emit(Record::ExecBg(&["bash".into(), "-c".into(), cmd]));
        }
    }

    if let Some(hook) = &cfg.hooks.post_cd {
        let argv = vec!["bash".into(), "-c".into(), post_cd_command(&r, hook)];
        emitter.emit(Record::Exec(&argv));
    }

    // Decide what to launch (claude | codex | nothing).
    if let Some(argv) = compose_editor_launch(&r, &flags, first_entry) {
        emitter.emit(Record::Exec(&argv));
    }
    Ok(())
}

/// Assemble the `claude` launch argv, mirroring the original `_cw_enter`:
///   pr + prompt → claude [--continue] --name <branch> --from-pr <pr> <prompt>
///   pr, no prompt → claude --name <branch> --from-pr <pr>
///   no pr, prompt → claude [--continue] <prompt>
///   nothing actionable → no launch (bare re-entry just cd's)
/// `--continue` is one more flag that AUGMENTS the argv (it does not replace
/// the rest); it's auto-added on re-entry-with-prompt and whenever the user
/// passes `--continue`. `--name`/`--from-pr` appear only for PR sessions.
fn compose_editor_launch(
    r: &resolve::Resolved,
    flags: &LaunchFlags,
    first_entry: bool,
) -> Option<Vec<String>> {
    let has_prompt = flags.prompt.is_some();
    let pr = flags.pr_override.or(r.pr);

    // Nothing to act on → just enter the workspace (bare `cw 8564`).
    if pr.is_none() && !has_prompt {
        return None;
    }

    // Original auto-continues on re-entry that carries a prompt; an explicit
    // --continue always applies. A first entry (fresh workspace) never auto-
    // continues — there's no prior session to resume.
    let cont = flags.continue_session || (!first_entry && has_prompt);

    let mut argv = vec!["claude".into()];
    if cont {
        argv.push("--continue".into());
    }
    if let Some(n) = pr {
        // PR-keyed session name like the original (`#<num> …`), kept scannable
        // across many sessions. We append the branch (already in hand, no extra
        // gh call) for description; the original appended the PR title from an
        // external cache — `#<num> <branch>` serves the same "which PR is this"
        // purpose without a network round-trip on every launch.
        argv.push("--name".into());
        match &r.branch {
            Some(branch) => argv.push(format!("#{n} {branch}")),
            None => argv.push(format!("#{n}")),
        }
        argv.push("--from-pr".into());
        argv.push(n.to_string());
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
    eprintln!("       cw --base <branch> <description>     # branch off an arbitrary base");
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
    // C3: a merged/closed PR with no existing worktree has nowhere to go —
    // don't spin up a fresh workspace for already-landed/abandoned work.
    // (When a worktree exists, resolve() finds it first and we never get here.)
    if pr.state != "OPEN" {
        anyhow::bail!(
            "PR #{number} ({}) is {} and not checked out in any workspace",
            pr.head_branch,
            pr.state.to_lowercase()
        );
    }
    Ok(Some(PrTarget {
        number,
        branch: pr.head_branch,
    }))
}

#[cfg(test)]
mod launch_tests {
    use super::*;
    use std::path::PathBuf;

    fn resolved(branch: Option<&str>, pr: Option<u32>) -> resolve::Resolved {
        resolve::Resolved {
            dir: PathBuf::from("/tmp/app_3"),
            number: Some(3),
            branch: branch.map(String::from),
            pr,
        }
    }

    fn flags(prompt: Option<&str>, pr: Option<u32>, cont: bool) -> LaunchFlags {
        LaunchFlags {
            stack: false,
            continue_session: cont,
            pr_override: pr,
            prompt: prompt.map(String::from),
        }
    }

    #[test]
    fn busy_worktree_path_parsed_from_git_error() {
        // The recovery must extract the path from git's message even when that
        // worktree is detached mid-rebase (a branch lookup would miss it).
        let err = "git worktree add failed: fatal: 'feat' is already used by worktree at '/Users/t/Code/app_4'";
        assert_eq!(
            super::parse_busy_worktree_path(err),
            Some(std::path::PathBuf::from("/Users/t/Code/app_4"))
        );
        assert_eq!(super::parse_busy_worktree_path("some other error"), None);
    }

    #[test]
    fn first_entry_description_launches_claude_with_prompt_only() {
        let argv = compose_editor_launch(
            &resolved(Some("fix-bug"), None),
            &flags(Some("fix the bug"), None, false),
            true,
        );
        assert_eq!(argv, Some(vec!["claude".into(), "fix the bug".into()]));
    }

    #[test]
    fn first_entry_pr_create_has_no_continue() {
        let argv = compose_editor_launch(
            &resolved(Some("feat"), None),
            &flags(None, Some(7543), false),
            true,
        );
        assert_eq!(
            argv,
            Some(vec![
                "claude".into(),
                "--name".into(),
                "#7543 feat".into(),
                "--from-pr".into(),
                "7543".into()
            ])
        );
    }

    #[test]
    fn reentry_with_prompt_auto_continues() {
        let argv = compose_editor_launch(
            &resolved(Some("feat"), None),
            &flags(Some("keep going"), None, false),
            false,
        );
        assert_eq!(
            argv,
            Some(vec![
                "claude".into(),
                "--continue".into(),
                "keep going".into()
            ])
        );
    }

    #[test]
    fn reentry_no_prompt_with_pr_resumes_without_continue() {
        // Bare `cw <N>` into a PR'd workspace resumes via --from-pr (the
        // regression was launching nothing), and does NOT auto --continue.
        let argv = compose_editor_launch(
            &resolved(Some("feat"), Some(42)),
            &flags(None, None, false),
            false,
        );
        assert_eq!(
            argv,
            Some(vec![
                "claude".into(),
                "--name".into(),
                "#42 feat".into(),
                "--from-pr".into(),
                "42".into()
            ])
        );
    }

    #[test]
    fn reentry_no_prompt_no_pr_launches_nothing() {
        let argv = compose_editor_launch(
            &resolved(Some("feat"), None),
            &flags(None, None, false),
            false,
        );
        assert_eq!(argv, None);
    }

    #[test]
    fn explicit_continue_augments_pr_and_prompt() {
        // The regression: --continue dropped --from-pr/--name/prompt. It must
        // augment them instead.
        let argv = compose_editor_launch(
            &resolved(Some("feat"), Some(9)),
            &flags(Some("review"), None, true),
            false,
        );
        assert_eq!(
            argv,
            Some(vec![
                "claude".into(),
                "--continue".into(),
                "--name".into(),
                "#9 feat".into(),
                "--from-pr".into(),
                "9".into(),
                "review".into()
            ])
        );
    }
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

/// True if a `git worktree add` failure was because the branch is already
/// checked out in another worktree (git: "is already used by worktree at ...").
fn is_busy_worktree_error(err: &str) -> bool {
    err.contains("already used by worktree")
}

/// Extract the worktree path from git's "already used by worktree at '<path>'"
/// message. Works regardless of the busy worktree's rebase/detached state —
/// unlike a branch→worktree lookup, which misses a detached (mid-rebase) one.
fn parse_busy_worktree_path(err: &str) -> Option<std::path::PathBuf> {
    const MARKER: &str = "already used by worktree at '";
    let start = err.find(MARKER)? + MARKER.len();
    let rest = &err[start..];
    let end = rest.find('\'')?;
    Some(std::path::PathBuf::from(&rest[..end]))
}
