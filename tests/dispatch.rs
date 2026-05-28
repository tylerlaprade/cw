use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
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

fn run_cw(repo: &Path, path: &str, extra_env: &[(&str, &str)], args: &[&str]) -> std::process::Output {
    let mut cmd = Command::cargo_bin("cw").unwrap();
    cmd.current_dir(repo).env("PATH", path);
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    cmd.args(args).output().unwrap()
}

fn make_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

#[test]
fn open_target_emits_cd_title_and_exec_records() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("condor");
    init_repo(&repo);
    commit_file(&repo, "README.md", "root\n", "root");

    let ws = tmp.path().join("condor_11");
    git(
        &repo,
        &[
            "worktree",
            "add",
            ws.to_str().unwrap(),
            "-b",
            "br-11",
            "develop",
        ],
    );

    let out = run_cw(
        &repo,
        "/usr/bin:/bin",
        &[("CW_WRAPPER", "1")],
        &["open", "11"],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let canonical_ws = fs::canonicalize(&ws).unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(&format!("CW\tCD\t{}", canonical_ws.display())));
    assert!(stdout.contains("CW\tTITLE\t#11"));
    assert!(stdout.contains("CW\tEXEC\tcw\tserve\tstart\t--open"));
}

#[test]
fn open_without_args_uses_current_workspace() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("condor");
    init_repo(&repo);
    commit_file(&repo, "README.md", "root\n", "root");

    let ws = tmp.path().join("condor_12");
    git(
        &repo,
        &[
            "worktree",
            "add",
            ws.to_str().unwrap(),
            "-b",
            "br-12",
            "develop",
        ],
    );

    let out = run_cw(
        &ws,
        "/usr/bin:/bin",
        &[("CW_WRAPPER", "1")],
        &["open"],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let canonical_ws = fs::canonicalize(&ws).unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(&format!("CW\tCD\t{}", canonical_ws.display())));
    assert!(stdout.contains("CW\tTITLE\t#12"));
    assert!(stdout.contains("CW\tEXEC\tcw\tserve\tstart\t--open"));
}

#[test]
fn numeric_token_without_workspace_or_pr_errors_instead_of_creating_branch() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("cw");
    init_repo(&repo);
    commit_file(&repo, "README.md", "root\n", "root");

    let mock_bin = tmp.path().join("bin");
    fs::create_dir(&mock_bin).unwrap();
    make_executable(
        &mock_bin.join("gh"),
        r#"#!/bin/sh
exit 1
"#,
    );
    let path = format!("{}:/usr/bin:/bin", mock_bin.display());

    let out = run_cw(&repo, &path, &[], &["8622"]);
    assert!(!out.status.success(), "expected failure");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("numeric target"));

    let stem = repo.file_name().unwrap().to_string_lossy();
    let sibling = repo.parent().unwrap().join(format!("{}_1", stem));
    assert!(!sibling.exists(), "unexpected workspace created at {}", sibling.display());
}
