//! `cw remove 0` must refuse to touch the repo root. The main worktree is
//! the canonical checkout — dropping it would blow away uncommitted local
//! state across every sibling workspace.

#[path = "common/support.rs"]
mod support;

use std::fs;
use support::{add_worktree, combined_output, commit_file, init_repo, run_cw, Runner};
use tempfile::TempDir;

#[test]
fn remove_zero_refuses() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("condor");
    init_repo(&repo);
    commit_file(&repo, "README.md", "root\n", "root");

    // A sibling workspace so the numeric dispatch has an alternate target and
    // the refusal is clearly about n=0, not "no workspaces exist".
    let ws = tmp.path().join("condor_3");
    add_worktree(&repo, &ws, "br-3", Runner::Rust);

    let out = run_cw(Runner::Rust, &repo, "/usr/bin:/bin", &[], &["remove", "0"]);
    assert!(
        !out.status.success(),
        "expected failure: {}",
        combined_output(&out)
    );

    let msg = combined_output(&out);
    assert!(
        msg.to_lowercase().contains("repo root") || msg.contains("workspace 0"),
        "error should name the protected target: {msg}"
    );

    // Repo root must still exist — the whole point of the refusal.
    assert!(repo.is_dir(), "repo root was removed");
    assert!(
        fs::read_dir(&repo).unwrap().next().is_some(),
        "repo root emptied"
    );
}
