//! `cw workspace list` must surface the main worktree as workspace 0.
//! Parity with the new `cw 0 → repo root` dispatch: if 0 is a valid target
//! everywhere else, it has to be a visible row in the inventory.

#[path = "common/support.rs"]
mod support;

use support::{add_worktree, combined_output, commit_file, init_repo, run_cw, Runner};
use tempfile::TempDir;

#[test]
fn workspace_list_shows_main_worktree_as_zero() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("condor");
    init_repo(&repo);
    commit_file(&repo, "README.md", "root\n", "root");

    let ws = tmp.path().join("condor_4");
    add_worktree(&repo, &ws, "br-4", Runner::Rust);

    let out = run_cw(
        Runner::Rust,
        &repo,
        "/usr/bin:/bin",
        &[],
        &["workspace", "list"],
    );
    assert!(out.status.success(), "{}", combined_output(&out));

    let stdout = String::from_utf8_lossy(&out.stdout);
    let has_zero_row = stdout.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("0 ") || trimmed.starts_with("0\t")
    });
    assert!(
        has_zero_row,
        "expected a row with N=0 for the main worktree.\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains("condor_4") || stdout.contains("br-4"),
        "expected sibling workspace to still be listed.\nstdout:\n{stdout}"
    );
}
