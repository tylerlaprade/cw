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

#[test]
fn removes_stale_no_unique_commits_workspace() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("condor");
    init_repo(&repo);
    commit_file(&repo, "README", "x\n", "root");

    // A workspace on a branch with no unique commits vs develop is removable
    // (its work is preserved on the base) — the real-deletion counterpart to
    // the fresh-skip test above.
    let ws = tmp.path().join("condor_6");
    git(
        &repo,
        &[
            "worktree",
            "add",
            ws.to_str().unwrap(),
            "-b",
            "br-6",
            "develop",
        ],
    );

    // Backdate the dir mtime well past the 48h fresh-skip so cleanup treats it
    // as genuinely stale rather than just-created.
    let touched = Command::new("touch")
        .args(["-t", "202001010000"])
        .arg(&ws)
        .status()
        .unwrap();
    assert!(touched.success(), "failed to backdate {}", ws.display());

    // Non-dry-run cleanup: a CLEAN (no-unique-work, no active session) stale
    // workspace is removed without --force.
    let out = Command::cargo_bin("cw")
        .unwrap()
        .current_dir(&repo)
        .env("PATH", "/usr/bin:/bin")
        .args(["cleanup"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cw cleanup failed: {}",
        combined_output(&out)
    );

    assert!(
        !ws.exists(),
        "stale workspace should have been removed:\n{}",
        combined_output(&out)
    );
    // Its branch (no unique work) should be pruned too.
    let branches = Command::new("git")
        .args(["branch", "--list", "br-6"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&branches.stdout).trim().is_empty(),
        "br-6 should have been deleted; still present:\n{}",
        String::from_utf8_lossy(&branches.stdout)
    );
}
