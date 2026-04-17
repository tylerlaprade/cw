//! `cw triage`: actionable PRs + tickets dashboard.

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

    // Jira project: derive from current branch prefix (e.g. CSC-1234 → CSC).
    let project = current_branch_prefix(root).unwrap_or_default();

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

    render::render(&prs, &tickets, args.verbose, terminal::columns());

    if !errors.is_empty() {
        eprintln!();
        for e in errors {
            eprintln!("warn: {e}");
        }
    }
    Ok(())
}

fn current_branch_prefix(dir: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    // Match e.g. CSC-1234, FOO-42 — an uppercase prefix before a hyphen-digit pair.
    let re = regex::Regex::new(r"^([A-Z]+)-\d+").ok()?;
    re.captures(&s)?.get(1).map(|m| m.as_str().to_string())
}
