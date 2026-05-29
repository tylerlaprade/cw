#[path = "common/dispatch_cases.rs"]
mod dispatch_cases;
#[path = "common/support.rs"]
mod support;

use std::fs;
use std::path::Path;
use std::process::Command;
use support::{
    add_worktree, combined_output, commit_file, git, init_repo, make_executable, run_cw, Runner,
};

#[test]
fn open_target_emits_cd_title_and_exec_records() {
    dispatch_cases::open_target_emits_cd_title_and_exec_records(Runner::Rust);
}

#[test]
fn open_without_args_uses_current_workspace() {
    dispatch_cases::open_without_args_uses_current_workspace(Runner::Rust);
}

#[test]
fn numeric_token_without_workspace_or_pr_errors_instead_of_creating_branch() {
    dispatch_cases::numeric_token_without_workspace_or_pr_errors_instead_of_creating_branch(
        Runner::Rust,
    );
}

#[test]
fn description_create_launches_claude_with_prompt() {
    dispatch_cases::description_create_launches_claude_with_prompt(Runner::Rust);
}

fn git_output(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?} failed in {}\n{}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn existing_branch_token_creates_worktree_without_prompt() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = tmp.path().join("condor");
    init_repo(&repo);
    commit_file(&repo, "README.md", "root\n", "root");
    git(&repo, &["checkout", "-b", "fix-bug", "--quiet"]);
    commit_file(&repo, "bug.txt", "fixed\n", "fix bug");
    git(&repo, &["checkout", "develop", "--quiet"]);

    let out = run_cw(
        Runner::Rust,
        &repo,
        "/usr/bin:/bin",
        &[("CW_WRAPPER", "1")],
        &["fix-bug"],
    );
    assert!(out.status.success(), "{}", combined_output(&out));

    let ws = tmp.path().join("condor_1");
    assert!(ws.is_dir(), "expected workspace at {}", ws.display());
    assert_eq!(
        git_output(&ws, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "fix-bug"
    );
    assert_eq!(fs::read_to_string(ws.join("bug.txt")).unwrap(), "fixed\n");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("CW\tEXEC\tclaude\tfix-bug"),
        "branch checkout must not launch the branch name as a prompt:\n{stdout}"
    );
}

#[test]
fn pr_create_fetches_remote_branch_before_worktree_add() {
    let tmp = tempfile::TempDir::new().unwrap();
    let origin = tmp.path().join("origin.git");
    let seed = tmp.path().join("seed");
    let repo = tmp.path().join("condor");

    let st = Command::new("git")
        .args(["init", "--bare", origin.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(st.success());
    init_repo(&seed);
    commit_file(&seed, "README.md", "root\n", "root");
    git(
        &seed,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&seed, &["push", "-u", "origin", "develop"]);
    git(&seed, &["checkout", "-b", "feature/pr-branch", "--quiet"]);
    commit_file(&seed, "pr.txt", "remote\n", "pr branch");
    git(&seed, &["push", "origin", "feature/pr-branch"]);

    let st = Command::new("git")
        .args([
            "clone",
            "--single-branch",
            "--branch",
            "develop",
            origin.to_str().unwrap(),
            repo.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(st.success());
    git(&repo, &["config", "user.email", "test@test.local"]);
    git(&repo, &["config", "user.name", "Test"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    let missing = Command::new("git")
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            "refs/remotes/origin/feature/pr-branch",
        ])
        .current_dir(&repo)
        .status()
        .unwrap();
    assert!(
        !missing.success(),
        "test setup unexpectedly fetched PR branch"
    );

    let mock_bin = tmp.path().join("bin");
    fs::create_dir(&mock_bin).unwrap();
    make_executable(
        &mock_bin.join("gh"),
        r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "view" ] && [ "$3" = "1234" ]; then
    printf 'OPEN\tfeature/pr-branch\n'
    exit 0
fi
exit 1
"#,
    );
    let path = format!("{}:/usr/bin:/bin", mock_bin.display());

    let out = run_cw(
        Runner::Rust,
        &repo,
        &path,
        &[("CW_WRAPPER", "1")],
        &["1234"],
    );
    assert!(out.status.success(), "{}", combined_output(&out));

    let ws = tmp.path().join("condor_1");
    assert_eq!(fs::read_to_string(ws.join("pr.txt")).unwrap(), "remote\n");
    assert_eq!(
        git_output(&ws, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "feature/pr-branch"
    );
}

#[test]
fn create_from_linked_worktree_uses_shared_git_dir_for_claim_lock() {
    let tmp = tempfile::TempDir::new().unwrap();
    let repo = tmp.path().join("app");
    init_repo(&repo);
    commit_file(&repo, "README.md", "root\n", "root");
    let linked = tmp.path().join("app_1");
    add_worktree(&repo, &linked, "linked-branch", Runner::Rust);

    let out = run_cw(
        Runner::Rust,
        &linked,
        "/usr/bin:/bin",
        &[("CW_WRAPPER", "1")],
        &["new", "work"],
    );
    assert!(out.status.success(), "{}", combined_output(&out));
    assert!(
        tmp.path().join("app_2").is_dir(),
        "expected second workspace from linked-worktree create"
    );
}
