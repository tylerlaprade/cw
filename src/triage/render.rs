//! Combined rendering for triage output.

use super::{gh::Pr, jira::Ticket};
use owo_colors::OwoColorize;

pub fn render(prs: &[Pr], tickets: &[Ticket], _verbose: bool, cols: usize) {
    if tickets.is_empty() && prs.is_empty() {
        println!("{} nothing actionable", "✓".green());
        return;
    }

    if !tickets.is_empty() {
        println!("{}", "Jira".bold().underline());
        for t in tickets {
            let status_c = match t.status.as_str() {
                "Failed QA" => t.status.red().to_string(),
                "To Do" => t.status.yellow().to_string(),
                _ => t.status.dimmed().to_string(),
            };
            println!(
                "  {:<10}  {:<14}  {}",
                t.key.bold(),
                status_c,
                truncate(&t.summary, cols.saturating_sub(32))
            );
        }
        println!();
    }

    if !prs.is_empty() {
        println!("{}", "Pull Requests".bold().underline());
        for pr in prs {
            let flags = pr_flags(pr);
            let flag_str = if flags.is_empty() {
                "".to_string()
            } else {
                format!(" [{}]", flags.join(" "))
            };
            println!(
                "  #{:<5} {:<12} {}{}",
                pr.number.bold(),
                state_badge(pr),
                truncate(&pr.title, cols.saturating_sub(30)),
                flag_str.dimmed()
            );
        }
    }
}

fn state_badge(pr: &Pr) -> String {
    match pr.merge_state.as_str() {
        "CLEAN" => "ready".green().to_string(),
        "BLOCKED" => "blocked".red().to_string(),
        "BEHIND" => "behind".yellow().to_string(),
        "DIRTY" => "conflict".red().to_string(),
        "UNSTABLE" => "unstable".yellow().to_string(),
        "HAS_HOOKS" | "" => "-".dimmed().to_string(),
        other => other.to_string(),
    }
}

fn pr_flags(pr: &Pr) -> Vec<String> {
    let mut out = Vec::new();
    if pr.is_draft {
        out.push("draft".to_string());
    }
    match pr.review_decision.as_str() {
        "CHANGES_REQUESTED" => out.push("changes requested".to_string()),
        "APPROVED" => out.push("approved".to_string()),
        _ => {}
    }
    if !pr.failing_checks.is_empty() {
        let n = pr.failing_checks.len();
        out.push(format!("{n} failing"));
    }
    out
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut iter = s.chars();
        let head: String = iter.by_ref().take(n.saturating_sub(1)).collect();
        format!("{head}…")
    }
}
