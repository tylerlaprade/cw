//! Combined rendering for triage output.

use super::{actionability::ActionablePr, jira::Ticket};
use owo_colors::OwoColorize;

pub fn render(prs: &[ActionablePr], tickets: &[Ticket], verbose: bool, cols: usize) {
    if tickets.is_empty() && prs.is_empty() {
        println!("{} nothing actionable", "✓".green());
        return;
    }

    if !tickets.is_empty() {
        println!("{}", "Jira".bold().underline());
        for t in tickets {
            // Match case-insensitively (original lowercased before keying).
            let status_c = match t.status.to_lowercase().as_str() {
                "failed qa" => t.status.red().to_string(),
                "to do" => t.status.yellow().to_string(),
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
            println!(
                "  #{:<5} {}  {}",
                pr.number.bold(),
                truncate(&pr.title, cols.saturating_sub(30)),
                color_issues(&pr.issues),
            );
            if verbose && !pr.failed_checks.is_empty() {
                let names: Vec<String> = pr.failed_checks.iter().map(|(n, _)| n.clone()).collect();
                println!("        {} {}", "failed:".red(), names.join(", ").dimmed());
            }
        }
    }
}

/// Color each computed issue, matching the original palette.
fn color_issues(issues: &[String]) -> String {
    issues
        .iter()
        .map(|i| match i.as_str() {
            "conflict" | "failing ci" => i.red().to_string(),
            "failing ci*" => i.dimmed().to_string(),
            "changes requested" => i.yellow().to_string(),
            "ready to merge" => i.green().to_string(),
            s if s.ends_with("unresolved") => i.blue().to_string(),
            _ => i.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
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
