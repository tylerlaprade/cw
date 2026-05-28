//! `cw cleanup` fresh-workspace protection.
//!
//! Regression: a just-created workspace whose branch has no unique commits vs
//! the base branch must NOT be flagged as stale by `cw cleanup`. Before the
//! fix, `Entry::is_removable` returned true for any no-unique-commits branch,
//! which caused brand-new workspaces to be swept seconds after they were
//! created.

use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "git {:?} failed in {}",
        args,
        dir.display()
    );
}

fn init_repo(dir: &Path) {
    git(
        dir.parent().unwrap(),
        &["init", "--initial-branch=develop", dir.to_str().unwrap()],
    );
    git(dir, &["config", "user.email", "test@test.local"]);
    git(dir, &["config", "user.name", "Test"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

fn commit_file(dir: &Path, rel: &str, contents: &str, msg: &str) {
    fs::write(dir.join(rel), contents).unwrap();
    git(dir, &["add", rel]);
    git(dir, &["commit", "-m", msg, "--quiet"]);
}

fn combined_output(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn does_not_flag_fresh_no_unique_commits_workspace() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("condor");
    init_repo(&repo);
    commit_file(&repo, "README", "x\n", "root");

    let ws = tmp.path().join("condor_5");
    git(
        &repo,
        &[
            "worktree",
            "add",
            ws.to_str().unwrap(),
            "-b",
            "br-5",
            "develop",
        ],
    );

    let out = Command::cargo_bin("cw")
        .unwrap()
        .current_dir(&repo)
        .env("PATH", "/usr/bin:/bin")
        .args(["cleanup", "--dry-run"])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "cw cleanup --dry-run failed: {}",
        combined_output(&out)
    );

    let text = combined_output(&out);
    assert!(
        !text.contains("No unique commits"),
        "fresh workspace should not be flagged as stale:\n{}",
        text
    );
    assert!(
        text.contains("no stale workspaces"),
        "expected 'no stale workspaces' in output:\n{}",
        text
    );
    assert!(ws.is_dir(), "workspace dir vanished during --dry-run");
}
