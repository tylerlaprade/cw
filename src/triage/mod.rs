//! `cw triage`: actionable PRs + tickets dashboard.

pub mod actionability;
pub mod actions;
pub mod gh;
pub mod jira;
pub mod render;

use crate::cli::TriageArgs;
use crate::config;
use crate::util::terminal;
use anyhow::Result;

pub fn run(args: TriageArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let cfg = config::discover::load(&cwd)?;
    let root = cfg
        .runtime
        .repo_root
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("not inside a git repo"))?;
    let base = cfg.runtime.base_branch.clone();

    // A configured `[triage] jira_project` wins (a branch-independent personal
    // dashboard, like the original's fixed project); otherwise derive the key
    // from the current branch's PR description, then the branch name.
    let project = cfg
        .triage
        .jira_project
        .clone()
        .or_else(|| current_pr_project(root))
        .or_else(|| current_branch_prefix(root))
        .unwrap_or_default();

    // Statuses to surface: configured, else generic Jira built-ins (the
    // original's company-custom "Failed QA"/"To Do" returned nothing elsewhere).
    let statuses = if cfg.triage.jira_statuses.is_empty() {
        vec!["To Do".to_string(), "In Progress".to_string()]
    } else {
        cfg.triage.jira_statuses.clone()
    };

    // Honor the acli integration toggle: explicit `false` disables Jira; unset
    // autodetects from $PATH (like the original's `command -v acli` skip).
    let acli = cfg
        .integrations
        .acli
        .unwrap_or_else(|| crate::util::in_path("acli"));

    // Fan out: PRs and tickets in parallel. Keep the raw join results so a
    // panicking worker degrades to an error line instead of aborting triage.
    let (prs_join, tickets_join) = std::thread::scope(|s| {
        let p = s.spawn(|| gh::list_my_open_prs(root, &base));
        let t = s.spawn(move || {
            if !acli || project.is_empty() {
                Ok::<Vec<jira::Ticket>, anyhow::Error>(Vec::new())
            } else {
                jira::my_actionable_tickets(&project, &statuses)
            }
        });
        (p.join(), t.join())
    });

    let mut errors = Vec::new();
    let prs = match prs_join {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            errors.push(format!("gh: {e:#}"));
            Vec::new()
        }
        Err(_) => {
            errors.push("gh: worker thread panicked".to_string());
            Vec::new()
        }
    };
    let mut tickets = match tickets_join {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            errors.push(format!("jira: {e:#}"));
            Vec::new()
        }
        Err(_) => {
            errors.push("jira: worker thread panicked".to_string());
            Vec::new()
        }
    };
    // Sort tickets by key (matching the original `tickets.sort()`).
    tickets.sort_by(|a, b| a.key.cmp(&b.key));

    // owner/repo backs both the feedback query and the PR hyperlinks in render.
    let owner_repo = gh::repo_owner_name(root);

    // Classify PRs into actionable issues. Needs branch-protection required
    // checks + the GraphQL review-feedback payload, both keyed by owner/repo.
    // In --verbose, the feedback query also pulls comment/review bodies.
    let actionable = if prs.is_empty() {
        Vec::new()
    } else {
        let numbers: Vec<u32> = prs.iter().map(|p| p.number).collect();
        let (required, feedback) = match &owner_repo {
            Some((owner, repo)) => (
                gh::fetch_required_checks(root, owner, repo, &base),
                gh::fetch_feedback(root, owner, repo, &numbers, args.verbose),
            ),
            None => (std::collections::HashSet::new(), serde_json::json!({})),
        };
        actionability::actionable_prs(&prs, &feedback, &required, args.verbose)
    };

    if !errors.is_empty() {
        if actionable.is_empty() && tickets.is_empty() {
            for e in errors {
                eprintln!("warn: {e}");
            }
            return Ok(());
        }
        eprintln!();
        for e in &errors {
            eprintln!("warn: {e}");
        }
    }

    render::render(
        &actionable,
        &tickets,
        args.verbose,
        terminal::columns(),
        owner_repo.as_ref(),
        cfg.triage.jira_site.as_deref(),
    );
    Ok(())
}

fn current_branch_name(dir: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let branch = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        None
    } else {
        Some(branch)
    }
}

fn current_pr_project(dir: &std::path::Path) -> Option<String> {
    let branch = current_branch_name(dir)?;
    let out = std::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--head",
            &branch,
            "--state",
            "all",
            "--json",
            "body",
            "-q",
            ".[0].body // \"\"",
        ])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let body = String::from_utf8(out.stdout).ok()?;
    extract_project_key(&body)
}

fn current_branch_prefix(dir: &std::path::Path) -> Option<String> {
    extract_project_key(&current_branch_name(dir)?)
}

fn extract_project_key(text: &str) -> Option<String> {
    let re = regex::Regex::new(r"([A-Z]+)-\d+").ok()?;
    re.captures(text)?.get(1).map(|m| m.as_str().to_string())
}
