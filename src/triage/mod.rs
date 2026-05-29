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

    // Prefer ticket keys from the current branch's PR description, and fall
    // back to the branch name when there is no PR body signal.
    let project = current_pr_project(root)
        .or_else(|| current_branch_prefix(root))
        .unwrap_or_default();

    // Fan out: PRs and tickets in parallel.
    let (prs_res, tickets_res) = std::thread::scope(|s| {
        let p = s.spawn(|| gh::list_my_open_prs(root, &base));
        let t = s.spawn(move || {
            if project.is_empty() {
                Ok::<Vec<jira::Ticket>, anyhow::Error>(Vec::new())
            } else {
                jira::my_actionable_tickets(&project)
            }
        });
        (p.join().unwrap(), t.join().unwrap())
    });

    let mut errors = Vec::new();
    let prs = prs_res.unwrap_or_else(|e| {
        errors.push(format!("gh: {e:#}"));
        Vec::new()
    });
    let tickets = tickets_res.unwrap_or_else(|e| {
        errors.push(format!("jira: {e:#}"));
        Vec::new()
    });

    // Classify PRs into actionable issues. Needs branch-protection required
    // checks + the GraphQL review-feedback payload, both keyed by owner/repo.
    let actionable = if prs.is_empty() {
        Vec::new()
    } else {
        let numbers: Vec<u32> = prs.iter().map(|p| p.number).collect();
        let (required, feedback) = match gh::repo_owner_name(root) {
            Some((owner, repo)) => (
                gh::fetch_required_checks(root, &owner, &repo, &base),
                gh::fetch_feedback(root, &owner, &repo, &numbers),
            ),
            None => (std::collections::HashSet::new(), serde_json::json!({})),
        };
        actionability::actionable_prs(&prs, &feedback, &required)
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

    render::render(&actionable, &tickets, args.verbose, terminal::columns());
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
