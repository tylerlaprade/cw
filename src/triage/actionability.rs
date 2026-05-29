//! PR actionability classification — a faithful port of the original
//! `format-triage.py` issue derivation. Given each PR's raw fields plus the
//! GraphQL review-feedback payload, compute the per-PR issue list
//! (`conflict`, `failing ci` / `failing ci*`, `changes requested`,
//! `N unresolved`, `ready to merge`) and skip auto-merge PRs.

use super::gh::Pr;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Logins matching any of these substrings are treated as bots.
const BOT_PATTERNS: [&str; 3] = ["bot", "[bot]", "github-actions"];
/// Bots that post automated PR-level comments but also review code threads.
const PR_COMMENT_BOTS: [&str; 1] = ["graphite-app"];

fn is_bot(login: &str) -> bool {
    if login.is_empty() {
        return true;
    }
    let l = login.to_lowercase();
    BOT_PATTERNS.iter().any(|p| l.contains(p))
}

/// One piece of verbose feedback to display: a review-thread comment or an
/// unanswered PR/review-level comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailKind {
    Thread,
    Comment,
}

#[derive(Debug, Clone)]
pub struct Detail {
    pub kind: DetailKind,
    pub author: String,
    pub body: String,
    pub time: String,
}

/// Per-PR review feedback derived from the GraphQL payload.
#[derive(Debug, Clone, Default)]
pub struct PrFeedback {
    pub unresolved_threads: usize,
    /// Logins of reviewers with comments/reviews after my last activity.
    pub unanswered_authors: Vec<String>,
    /// Logins whose latest review state is CHANGES_REQUESTED.
    pub change_requesters: Vec<String>,
    /// Verbose-only: the feedback bodies to render (empty otherwise).
    pub details: Vec<Detail>,
}

/// A PR that needs attention, with its computed issue list.
#[derive(Debug, Clone)]
pub struct ActionablePr {
    pub number: u32,
    pub title: String,
    pub issues: Vec<String>,
    /// (check name, details URL) for failed checks — used by verbose rendering.
    pub failed_checks: Vec<(String, String)>,
    /// Verbose-only feedback detail (thread + unanswered comment bodies).
    pub details: Vec<Detail>,
}

fn node_list<'a>(v: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    v.get(key)
        .and_then(|n| n.get("nodes"))
        .and_then(|n| n.as_array())
        .into_iter()
        .flatten()
}

/// `login` of `v[key].login` (key is "author" or "actor"), or "".
fn login_at<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key)
        .and_then(|a| a.get("login"))
        .and_then(|l| l.as_str())
        .unwrap_or("")
}

fn created_at(v: &Value) -> &str {
    v.get("createdAt").and_then(|c| c.as_str()).unwrap_or("")
}

/// Derive feedback for one PR from its GraphQL `pullRequest` node. When
/// `verbose`, also collect comment/thread bodies (`details`) for rendering.
pub fn compute_feedback(node: &Value, verbose: bool) -> PrFeedback {
    let my_login = login_at(node, "author");
    let mut details: Vec<Detail> = Vec::new();

    let mut unresolved_threads = 0usize;
    for t in node_list(node, "reviewThreads") {
        let resolved = t
            .get("isResolved")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        let outdated = t
            .get("isOutdated")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        if resolved || outdated {
            continue;
        }
        unresolved_threads += 1;
        if verbose {
            // Collect this thread's non-bot comments, then window to those after
            // my second-to-last own comment (the relevant tail of a long thread).
            let comments: Vec<(String, String, String)> = node_list(t, "comments")
                .filter_map(|c| {
                    let login = login_at(c, "author");
                    if is_bot(login) {
                        return None;
                    }
                    let body = c.get("body").and_then(|b| b.as_str()).unwrap_or("").trim();
                    Some((
                        login.to_string(),
                        body.to_string(),
                        created_at(c).to_string(),
                    ))
                })
                .collect();
            let my_idx: Vec<usize> = comments
                .iter()
                .enumerate()
                .filter(|(_, c)| c.0 == my_login)
                .map(|(i, _)| i)
                .collect();
            let start = if my_idx.len() >= 2 {
                my_idx[my_idx.len() - 2] + 1
            } else {
                0
            };
            for (author, body, time) in comments.into_iter().skip(start) {
                if body.is_empty() {
                    continue;
                }
                details.push(Detail {
                    kind: DetailKind::Thread,
                    author,
                    body,
                    time,
                });
            }
        }
    }

    // Most recent activity by the PR author (across comments, reviews, and
    // review-dismissal events) — the cutoff for "unanswered" feedback.
    let mut my_last = "";
    for c in node_list(node, "comments") {
        if login_at(c, "author") == my_login {
            my_last = my_last.max(created_at(c));
        }
    }
    for r in node_list(node, "reviews") {
        if login_at(r, "author") == my_login {
            my_last = my_last.max(created_at(r));
        }
    }
    for d in node_list(node, "timelineItems") {
        if login_at(d, "actor") == my_login {
            my_last = my_last.max(created_at(d));
        }
    }

    let mut unanswered_authors = Vec::new();
    for c in node_list(node, "comments") {
        let login = login_at(c, "author");
        if login != my_login
            && !is_bot(login)
            && !PR_COMMENT_BOTS.contains(&login)
            && created_at(c) > my_last
        {
            unanswered_authors.push(login.to_string());
            if verbose {
                let body = c.get("body").and_then(|b| b.as_str()).unwrap_or("").trim();
                if !body.is_empty() {
                    details.push(Detail {
                        kind: DetailKind::Comment,
                        author: login.to_string(),
                        body: body.to_string(),
                        time: created_at(c).to_string(),
                    });
                }
            }
        }
    }
    for r in node_list(node, "reviews") {
        let body = r.get("body").and_then(|b| b.as_str()).unwrap_or("").trim();
        let login = login_at(r, "author");
        if !body.is_empty() && login != my_login && !is_bot(login) && created_at(r) > my_last {
            unanswered_authors.push(login.to_string());
            if verbose {
                details.push(Detail {
                    kind: DetailKind::Comment,
                    author: login.to_string(),
                    body: body.to_string(),
                    time: created_at(r).to_string(),
                });
            }
        }
    }

    // Latest review state per author (chronological → last wins).
    let mut latest_state: HashMap<String, String> = HashMap::new();
    for r in node_list(node, "reviews") {
        let login = login_at(r, "author");
        let state = r.get("state").and_then(|s| s.as_str()).unwrap_or("");
        if !login.is_empty() && (state == "CHANGES_REQUESTED" || state == "APPROVED") {
            latest_state.insert(login.to_string(), state.to_string());
        }
    }
    let change_requesters = latest_state
        .into_iter()
        .filter(|(_, s)| s == "CHANGES_REQUESTED")
        .map(|(l, _)| l)
        .collect();

    PrFeedback {
        unresolved_threads,
        unanswered_authors,
        change_requesters,
        details,
    }
}

/// Compute the issue list for one PR. Returns None when the PR is auto-merging
/// or has no actionable issues.
pub fn classify(pr: &Pr, fb: &PrFeedback, required: &HashSet<String>) -> Option<ActionablePr> {
    // Graphite is already merging this — not actionable.
    if pr.auto_merge_request.is_some() {
        return None;
    }

    let mut issues: Vec<String> = Vec::new();

    // Dedup checks by name, keeping the most recent run.
    let mut latest: HashMap<&str, &super::gh::Check> = HashMap::new();
    for c in &pr.status_check_rollup {
        let keep = match latest.get(c.name.as_str()) {
            Some(prev) => {
                c.completed_at.as_deref().unwrap_or("") > prev.completed_at.as_deref().unwrap_or("")
            }
            None => true,
        };
        if keep {
            latest.insert(c.name.as_str(), c);
        }
    }
    let failed: Vec<(String, String)> = latest
        .values()
        .filter(|c| c.conclusion.as_deref() == Some("FAILURE"))
        .map(|c| (c.name.clone(), c.details_url.clone().unwrap_or_default()))
        .collect();
    let required_failed = failed.iter().any(|(n, _)| required.contains(n));
    let optional_failed = failed.iter().any(|(n, _)| !required.contains(n));

    if pr.mergeable == "CONFLICTING" {
        issues.push("conflict".to_string());
    }
    if required_failed {
        issues.push("failing ci".to_string());
    } else if optional_failed {
        issues.push("failing ci*".to_string());
    }

    // If re-review has been requested from every change-requester, their
    // feedback is no longer actionable (the ball is in their court).
    let mut re_requested: HashSet<&str> = HashSet::new();
    if pr.review_decision == "CHANGES_REQUESTED" {
        let pending: HashSet<String> = pr.review_requests.iter().filter_map(|r| r.id()).collect();
        let crs: HashSet<&str> = fb.change_requesters.iter().map(|s| s.as_str()).collect();
        if !crs.is_empty() && crs.iter().all(|c| pending.contains(*c)) {
            re_requested = crs;
        } else {
            issues.push("changes requested".to_string());
        }
    }

    let unanswered_active = fb
        .unanswered_authors
        .iter()
        .filter(|a| !re_requested.contains(a.as_str()))
        .count();
    let unresolved_total = fb.unresolved_threads + unanswered_active;
    if unresolved_total > 0 && !issues.iter().any(|i| i == "changes requested") {
        issues.push(format!("{unresolved_total} unresolved"));
    }

    if issues.is_empty() && pr.review_decision == "APPROVED" {
        issues.push("ready to merge".to_string());
    }

    if issues.is_empty() {
        return None;
    }
    Some(ActionablePr {
        number: pr.number,
        title: pr.title.clone(),
        issues,
        failed_checks: failed,
        details: fb.details.clone(),
    })
}

/// Classify every PR against the feedback payload, returning the actionable
/// ones sorted by number descending (matching the original).
pub fn actionable_prs(
    prs: &[Pr],
    feedback: &Value,
    required: &HashSet<String>,
    verbose: bool,
) -> Vec<ActionablePr> {
    let data = feedback.get("data");
    let mut out: Vec<ActionablePr> = prs
        .iter()
        .filter_map(|pr| {
            let node = data
                .and_then(|d| d.get(format!("pr{}", pr.number)))
                .and_then(|w| w.get("pullRequest"));
            let fb = match node {
                Some(n) if !n.is_null() => compute_feedback(n, verbose),
                _ => PrFeedback::default(),
            };
            classify(pr, &fb, required)
        })
        .collect();
    out.sort_by(|a, b| b.number.cmp(&a.number));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triage::gh::{Check, Pr, ReviewRequest};
    use std::collections::HashSet;

    fn pr() -> Pr {
        Pr::default()
    }
    fn check(name: &str, conclusion: &str) -> Check {
        Check {
            name: name.into(),
            conclusion: Some(conclusion.into()),
            ..Default::default()
        }
    }

    #[test]
    fn conflict_and_failing_required_ci() {
        let mut p = pr();
        p.mergeable = "CONFLICTING".into();
        p.status_check_rollup = vec![check("build", "FAILURE")];
        let required: HashSet<String> = ["build".to_string()].into_iter().collect();
        let a = classify(&p, &PrFeedback::default(), &required).unwrap();
        assert_eq!(a.issues, vec!["conflict", "failing ci"]);
    }

    #[test]
    fn optional_failing_uses_star() {
        let mut p = pr();
        p.status_check_rollup = vec![check("lint", "FAILURE")];
        let a = classify(&p, &PrFeedback::default(), &HashSet::new()).unwrap();
        assert_eq!(a.issues, vec!["failing ci*"]);
    }

    #[test]
    fn ready_to_merge_when_approved_and_clean() {
        let mut p = pr();
        p.review_decision = "APPROVED".into();
        let a = classify(&p, &PrFeedback::default(), &HashSet::new()).unwrap();
        assert_eq!(a.issues, vec!["ready to merge"]);
    }

    #[test]
    fn auto_merge_pr_is_skipped() {
        let mut p = pr();
        p.review_decision = "APPROVED".into();
        p.auto_merge_request = Some(serde_json::json!({"enabledAt": "x"}));
        assert!(classify(&p, &PrFeedback::default(), &HashSet::new()).is_none());
    }

    #[test]
    fn unresolved_counts_threads_plus_unanswered() {
        let fb = PrFeedback {
            unresolved_threads: 2,
            unanswered_authors: vec!["alice".into()],
            change_requesters: vec![],
            details: vec![],
        };
        let a = classify(&pr(), &fb, &HashSet::new()).unwrap();
        assert_eq!(a.issues, vec!["3 unresolved"]);
    }

    #[test]
    fn changes_requested_suppresses_unresolved_count() {
        let mut p = pr();
        p.review_decision = "CHANGES_REQUESTED".into();
        // bob requested changes and was NOT re-requested → "changes requested",
        // and the unresolved line is suppressed.
        let fb = PrFeedback {
            unresolved_threads: 1,
            unanswered_authors: vec!["bob".into()],
            change_requesters: vec!["bob".into()],
            details: vec![],
        };
        let a = classify(&p, &fb, &HashSet::new()).unwrap();
        assert_eq!(a.issues, vec!["changes requested"]);
    }

    #[test]
    fn re_requested_reviewer_feedback_is_excluded() {
        let mut p = pr();
        p.review_decision = "CHANGES_REQUESTED".into();
        p.review_requests = vec![ReviewRequest {
            login: Some("bob".into()),
            name: None,
        }];
        // Re-review requested from bob (the only change requester) → nothing
        // actionable remains.
        let fb = PrFeedback {
            unresolved_threads: 0,
            unanswered_authors: vec!["bob".into()],
            change_requesters: vec!["bob".into()],
            details: vec![],
        };
        assert!(classify(&p, &fb, &HashSet::new()).is_none());
    }

    #[test]
    fn compute_feedback_counts_unresolved_and_unanswered() {
        let node = serde_json::json!({
            "author": {"login": "me"},
            "reviewThreads": {"nodes": [
                {"isResolved": false, "isOutdated": false},
                {"isResolved": true,  "isOutdated": false},
                {"isResolved": false, "isOutdated": true}
            ]},
            "reviews": {"nodes": [
                {"state": "CHANGES_REQUESTED", "body": "fix this",
                 "author": {"login": "rev"}, "createdAt": "2026-01-02T00:00:00Z"}
            ]},
            "comments": {"nodes": [
                {"author": {"login": "me"},  "createdAt": "2026-01-01T00:00:00Z"},
                {"author": {"login": "rev"}, "createdAt": "2026-01-03T00:00:00Z"}
            ]},
            "timelineItems": {"nodes": []}
        });
        let fb = compute_feedback(&node, false);
        assert_eq!(fb.unresolved_threads, 1);
        // rev's review (after my last activity) + rev's later comment → 2.
        assert_eq!(fb.unanswered_authors.len(), 2);
        assert_eq!(fb.change_requesters, vec!["rev".to_string()]);
    }

    #[test]
    fn bot_and_self_comments_dont_count_as_unanswered() {
        let node = serde_json::json!({
            "author": {"login": "me"},
            "reviewThreads": {"nodes": []},
            "reviews": {"nodes": []},
            "comments": {"nodes": [
                {"author": {"login": "graphite-app"}, "createdAt": "2026-02-01T00:00:00Z"},
                {"author": {"login": "dependabot[bot]"}, "createdAt": "2026-02-01T00:00:00Z"},
                {"author": {"login": "me"}, "createdAt": "2026-02-02T00:00:00Z"}
            ]},
            "timelineItems": {"nodes": []}
        });
        let fb = compute_feedback(&node, false);
        assert!(fb.unanswered_authors.is_empty());
    }
}
