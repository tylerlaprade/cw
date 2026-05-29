//! Combined rendering for triage output.

use super::{
    actionability::{ActionablePr, DetailKind},
    jira::Ticket,
};
use once_cell::sync::Lazy;
use owo_colors::OwoColorize;
use regex::Regex;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn render(
    prs: &[ActionablePr],
    tickets: &[Ticket],
    verbose: bool,
    cols: usize,
    owner_repo: Option<&(String, String)>,
    jira_site: Option<&str>,
) {
    if tickets.is_empty() && prs.is_empty() {
        println!("{} nothing actionable", "✓".green());
        return;
    }

    // PRs first, then Jira — matching the original section order.
    if !prs.is_empty() {
        println!("{} ({})", "Pull Requests".bold().underline(), prs.len());
        for pr in prs {
            // PR number as an OSC-8 hyperlink to the GitHub PR page (always
            // available — no Graphite/company host needed). Pad by the VISIBLE
            // width so the escape sequences don't throw off alignment.
            let plain = format!("#{}", pr.number);
            let cell = match owner_repo {
                Some((o, r)) => {
                    hyperlink(&format!("https://github.com/{o}/{r}/pull/{}", pr.number), &plain)
                }
                None => plain.clone(),
            };
            let pad = " ".repeat(6usize.saturating_sub(plain.chars().count()));
            println!(
                "  {}{} {}  {}",
                cell.bold(),
                pad,
                truncate(&pr.title, cols.saturating_sub(30)),
                color_issues(&pr.issues),
            );
            if verbose {
                render_pr_verbose(pr);
            }
        }
        println!();
    }

    if !tickets.is_empty() {
        println!("{} ({})", "Jira".bold().underline(), tickets.len());
        for t in tickets {
            let key_cell = match jira_site {
                Some(site) => hyperlink(&format!("https://{site}/browse/{}", t.key), &t.key),
                None => t.key.clone(),
            };
            let pad = " ".repeat(10usize.saturating_sub(t.key.chars().count()));
            // Match case-insensitively (original lowercased before keying).
            let status_c = match t.status.to_lowercase().as_str() {
                "failed qa" => t.status.red().to_string(),
                "to do" => t.status.cyan().to_string(),
                _ => t.status.dimmed().to_string(),
            };
            println!(
                "  {}{}  {:<14}  {}",
                key_cell.bold(),
                pad,
                status_c,
                truncate(&t.summary, cols.saturating_sub(32))
            );
        }
        println!();
    }
}

/// Verbose per-PR detail: linked failed-check names, then each unresolved-thread
/// comment / unanswered comment with author, relative time, and cleaned body.
fn render_pr_verbose(pr: &ActionablePr) {
    if !pr.failed_checks.is_empty() {
        let names: Vec<String> = pr
            .failed_checks
            .iter()
            .map(|(n, url)| {
                if url.is_empty() {
                    n.clone()
                } else {
                    hyperlink(url, n)
                }
            })
            .collect();
        println!("        {} {}", "failed:".red(), names.join(", ").dimmed());
    }
    for d in &pr.details {
        let body = clean_body(&d.body);
        if body.is_empty() {
            continue;
        }
        let time = relative_time(&d.time);
        let suffix = if time.is_empty() {
            ":".to_string()
        } else {
            format!(" ({time}):")
        };
        match d.kind {
            DetailKind::Thread => {
                println!("        {} {}{}", "│".dimmed(), d.author.bold(), suffix.dimmed());
                for line in body.lines() {
                    println!("        {}   {}", "│".dimmed(), line);
                }
            }
            DetailKind::Comment => {
                println!("        {}{}", d.author.bold(), suffix.dimmed());
                for line in body.lines() {
                    println!("          {}", line);
                }
            }
        }
    }
}

/// An OSC-8 terminal hyperlink. Terminals without OSC-8 support render the text
/// only (the escapes are stripped), so this is always safe to emit.
fn hyperlink(url: &str, text: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
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

static RE_SUGGEST: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?s)```suggestion\n.*?```").unwrap());
static RE_FOOTER: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?s)\*Spotted by \[Graphite.*$").unwrap());
static RE_HTML: Lazy<Regex> = Lazy::new(|| Regex::new(r"<[^>]+>").unwrap());
static RE_BLANKS: Lazy<Regex> = Lazy::new(|| Regex::new(r"\n{3,}").unwrap());

const MAX_BODY_LINES: usize = 6;

/// Clean a comment body for terminal display: drop ```suggestion blocks and
/// Graphite footers, strip HTML, collapse blank-line runs, truncate to 6 lines.
/// A faithful port of format-triage.py's `clean_body`.
fn clean_body(text: &str) -> String {
    let t = RE_SUGGEST.replace_all(text, "");
    let t = RE_FOOTER.replace_all(&t, "");
    let t = RE_HTML.replace_all(&t, "");
    let t = RE_BLANKS.replace_all(&t, "\n\n");
    let t = t.trim();
    let lines: Vec<&str> = t.lines().collect();
    if lines.len() > MAX_BODY_LINES {
        format!("{}\n…", lines[..MAX_BODY_LINES].join("\n"))
    } else {
        t.to_string()
    }
}

/// Render an ISO-8601 UTC timestamp as "just now" / "Nm ago" / "Nh ago" /
/// "Nd ago". Empty string when the timestamp can't be parsed.
fn relative_time(iso: &str) -> String {
    let Some(ts) = parse_iso_epoch(iso) else {
        return String::new();
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(ts);
    let d = now - ts;
    if d < 60 {
        "just now".to_string()
    } else if d < 3600 {
        format!("{}m ago", d / 60)
    } else if d < 86400 {
        format!("{}h ago", d / 3600)
    } else {
        format!("{}d ago", d / 86400)
    }
}

/// Parse "YYYY-MM-DDTHH:MM:SS" (optionally with trailing Z/offset) to a Unix
/// epoch in seconds. Uses Howard Hinnant's days-from-civil algorithm so no
/// date crate is needed.
fn parse_iso_epoch(s: &str) -> Option<i64> {
    if s.len() < 19 {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: i64 = s.get(5..7)?.parse().ok()?;
    let day: i64 = s.get(8..10)?.parse().ok()?;
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    let min: i64 = s.get(14..16)?.parse().ok()?;
    let sec: i64 = s.get(17..19)?.parse().ok()?;
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12; // Mar=0..Feb=11
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hour * 3600 + min * 60 + sec)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_iso_epoch_matches_known_values() {
        // 2021-01-01T00:00:00Z = 1609459200.
        assert_eq!(parse_iso_epoch("2021-01-01T00:00:00Z"), Some(1609459200));
        // The Unix epoch itself.
        assert_eq!(parse_iso_epoch("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_epoch("nonsense"), None);
    }

    #[test]
    fn clean_body_strips_suggestion_html_and_truncates() {
        let raw = "line1\n```suggestion\nfoo\n```\n<b>bold</b> text\n\n\n\ntail";
        let out = clean_body(raw);
        assert!(!out.contains("suggestion"));
        assert!(!out.contains("<b>"));
        assert!(out.contains("bold"));
        // 3+ blank lines collapsed.
        assert!(!out.contains("\n\n\n"));
    }
}
