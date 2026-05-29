//! A1 critical-safety regression: the main worktree must never be treated as a
//! removable numbered workspace, even when its directory name matches
//! `{stem}_{N}` (e.g. a repo cloned into `app_2`). Without the guard,
//! `cw remove` / `cw remove 2` would map the main repo to "workspace 2" and
//! delete it.

#[path = "common/support.rs"]
mod support;

use std::fs;
use support::{add_worktree, combined_output, commit_file, init_repo, run_cw, Runner};
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

#[test]
fn remove_refuses_dirty_workspace_without_force() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("app");
    init_repo(&repo);
    commit_file(&repo, "README.md", "root\n", "root");

    // A real numbered workspace as a sibling worktree, with uncommitted work.
    let ws = tmp.path().join("app_3");
    add_worktree(&repo, &ws, "feat-3", Runner::Rust);
    std::fs::write(ws.join("README.md"), "uncommitted edit\n").unwrap();

    // `cw remove 3` WITHOUT --force must skip the dirty workspace and leave it
    // on disk. (run() exits 0 but reports DIRTY + "Skipping".)
    let out = run_cw(Runner::Rust, &repo, "/usr/bin:/bin", &[], &["remove", "3"]);
    let combined = combined_output(&out);
    assert!(
        ws.join("README.md").exists(),
        "dirty workspace must NOT be removed without --force\n{combined}"
    );
    assert!(
        combined.contains("DIRTY")
            || combined.to_lowercase().contains("skipping")
            || combined.to_lowercase().contains("uncommitted"),
        "expected a dirty/skip notice; got:\n{combined}"
    );
}

#[test]
fn force_remove_numeric_orphan_still_drops_configured_databases() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("app");
    init_repo(&repo);
    commit_file(&repo, "README.md", "root\n", "root");
    fs::write(
        repo.join(".devcli.toml"),
        r#"[workspace]
stem = "app"

[databases]
pattern = "app_{n}_{suffix}"
suffixes = ["qa"]
clone = "postgres"
"#,
    )
    .unwrap();

    let mock_bin = tmp.path().join("bin");
    fs::create_dir(&mock_bin).unwrap();
    let dropdb_log = tmp.path().join("dropdb.log");
    support::make_executable(
        &mock_bin.join("dropdb"),
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$DROPDB_LOG"
exit 0
"#,
    );
    let path = format!("{}:/usr/bin:/bin", mock_bin.display());

    let out = run_cw(
        Runner::Rust,
        &repo,
        &path,
        &[("DROPDB_LOG", dropdb_log.to_str().unwrap())],
        &["remove", "--force", "7"],
    );
    assert!(out.status.success(), "{}", combined_output(&out));
    let log = fs::read_to_string(&dropdb_log).unwrap();
    assert!(
        log.contains("--if-exists app_7_qa"),
        "expected orphan DB drop for workspace 7, got:\n{log}"
    );
}
