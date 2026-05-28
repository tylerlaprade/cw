//! A1 critical-safety regression: the main worktree must never be treated as a
//! removable numbered workspace, even when its directory name matches
//! `{stem}_{N}` (e.g. a repo cloned into `app_2`). Without the guard,
//! `cw remove` / `cw remove 2` would map the main repo to "workspace 2" and
//! delete it.

#[path = "common/support.rs"]
mod support;

use support::{combined_output, commit_file, init_repo, run_cw, Runner};
use tempfile::TempDir;

#[test]
fn main_worktree_named_like_a_workspace_resolves_to_zero() {
    let tmp = TempDir::new().unwrap();
    // Repo cloned into a dir whose name ends in _<N>; stem autodetects to "app".
    let repo = tmp.path().join("app_2");
    init_repo(&repo);
    commit_file(&repo, "README.md", "root\n", "root");

    // `cw workspace resolve 2` must map to the main worktree as number 0 — NOT a
    // phantom sibling workspace — so teardown's workspace-0 guard refuses it.
    let out = run_cw(
        Runner::Rust,
        &repo,
        "/usr/bin:/bin",
        &[],
        &["workspace", "resolve", "2", "--json"],
    );
    assert!(out.status.success(), "{}", combined_output(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"number\":0"),
        "main worktree named app_2 must resolve to number 0.\nstdout: {stdout}"
    );
}

#[test]
fn remove_refuses_main_worktree_named_like_a_workspace() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("app_2");
    init_repo(&repo);
    commit_file(&repo, "README.md", "root\n", "root");

    // `cw remove 2` from inside the main repo must refuse (workspace 0 guard),
    // leaving the repo intact.
    let out = run_cw(
        Runner::Rust,
        &repo,
        "/usr/bin:/bin",
        &[],
        &["remove", "2", "--force"],
    );
    assert!(
        !out.status.success(),
        "expected `cw remove 2` to refuse the main repo.\n{}",
        combined_output(&out)
    );
    assert!(
        repo.join(".git").exists() && repo.join("README.md").exists(),
        "main repo must still exist after refused removal"
    );
}
