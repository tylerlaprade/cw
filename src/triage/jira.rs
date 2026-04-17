//! Jira ticket fetching via `acli`.

use anyhow::{Context, Result};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct Ticket {
    pub key: String,
    pub summary: String,
    pub status: String,
}

pub fn my_actionable_tickets(project: &str) -> Result<Vec<Ticket>> {
    let jql = format!(
        r#"project = {project} AND assignee = currentUser() AND status IN ("Failed QA", "To Do")"#
    );
    let out = Command::new("acli")
        .args([
            "jira",
            "workitem",
            "search",
            "--jql",
            &jql,
            "--fields",
            "key,summary,status",
            "--json",
        ])
        .output()
        .context("acli jira workitem search")?;
    if !out.status.success() {
        anyhow::bail!(
            "acli jira workitem search failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(parse_acli_json(&String::from_utf8_lossy(&out.stdout)))
}

/// Parse `acli jira` JSON without pulling serde_json. The output is an
/// array of objects; we care about `key` / `fields.summary` / `fields.status.name`.
/// When the shape shifts, we fall back to empty results rather than panic.
fn parse_acli_json(text: &str) -> Vec<Ticket> {
    let mut out = Vec::new();
    let text = text.trim();
    if !text.starts_with('[') {
        return out;
    }
    // Very light parse: scan for "key": followed by a quoted string, "summary":,
    // and "status": name:. Good enough for a one-line-per-ticket extract.
    let mut i = 0;
    let bytes = text.as_bytes();
    loop {
        let Some(key_pos) = find_from(text, "\"key\"", i) else {
            break;
        };
        let Some(key) = read_json_string_after(text, key_pos + 5) else {
            break;
        };

        // Next, within this object, find summary and status.
        let obj_end = find_balanced_close(bytes, key_pos).unwrap_or(text.len());
        let slice = &text[key_pos..obj_end];
        let summary = find_from(slice, "\"summary\"", 0)
            .and_then(|p| read_json_string_after(slice, p + 9))
            .unwrap_or_default();
        let status = find_from(slice, "\"name\"", 0)
            .and_then(|p| read_json_string_after(slice, p + 6))
            .unwrap_or_default();

        out.push(Ticket {
            key: key.clone(),
            summary,
            status,
        });
        i = obj_end;
    }
    out
}

fn find_from(text: &str, pat: &str, from: usize) -> Option<usize> {
    text[from..].find(pat).map(|p| p + from)
}

fn read_json_string_after(text: &str, from: usize) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < bytes.len() && bytes[i] != b'"' {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let start = i + 1;
    let mut j = start;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b'"' => return Some(text[start..j].to_string()),
            _ => j += 1,
        }
    }
    None
}

fn find_balanced_close(bytes: &[u8], from: usize) -> Option<usize> {
    // Find the enclosing object's closing brace from inside it.
    let mut depth = 1;
    let mut i = from;
    // Walk back to the opening brace.
    while i > 0 && bytes[i] != b'{' {
        i -= 1;
    }
    i += 1;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            b'"' => {
                // Skip string.
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}
