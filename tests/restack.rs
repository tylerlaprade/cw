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

fn git_output(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?} failed in {}",
        args,
        dir.display()
    );
    String::from_utf8(out.stdout).unwrap()
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

fn make_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

fn run_cw(
    repo: &Path,
    path: &str,
    extra_env: &[(&str, &str)],
    args: &[&str],
) -> std::process::Output {
    let mut cmd = Command::cargo_bin("cw").unwrap();
    cmd.current_dir(repo).env("PATH", path);
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    cmd.args(args).output().unwrap()
}

#[test]
fn restack_target_emits_cd_and_suppresses_successful_graphite_noise() {
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

    let mock_bin = tmp.path().join("bin");
    fs::create_dir(&mock_bin).unwrap();
    let mock_log = tmp.path().join("mock.log");
    make_executable(
        &mock_bin.join("gt"),
        r#"#!/bin/sh
echo "$PWD :: $*" >> "$MOCK_LOG"
case "$1" in
  get) echo "NOISY GT GET"; exit 0 ;;
  r) echo "NOISY GT R"; exit 0 ;;
  continue) exit 0 ;;
esac
exit 0
"#,
    );

    let path = format!("{}:/usr/bin:/bin", mock_bin.display());
    let out = run_cw(
        &repo,
        &path,
        &[
            ("CW_WRAPPER", "1"),
            ("MOCK_LOG", mock_log.to_str().unwrap()),
        ],
        &["restack", "11"],
    );
    assert!(out.status.success());

    let canonical_ws = fs::canonicalize(&ws).unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let log = fs::read_to_string(&mock_log).unwrap();
    assert!(
        stdout.contains(&format!("CW\tCD\t{}", canonical_ws.display())),
        "stdout={stdout:?}\nstderr={:?}\nlog={log:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!stdout.contains("NOISY GT GET"));
    assert!(!stdout.contains("NOISY GT R"));

    assert!(log.contains(&format!(
        "{} :: get --no-interactive",
        canonical_ws.display()
    )));
    assert!(log.contains(&format!("{} :: r --quiet", canonical_ws.display())));
}

fn make_rebase_conflict(repo: &Path) {
    commit_file(repo, "demo.txt", "base\n", "base");
    git(repo, &["checkout", "-b", "feature", "--quiet"]);
    commit_file(repo, "demo.txt", "feature\n", "feature");
    git(repo, &["checkout", "develop", "--quiet"]);
    commit_file(repo, "demo.txt", "develop\n", "develop");
    git(repo, &["checkout", "feature", "--quiet"]);
    let status = Command::new("git")
        .args(["rebase", "develop"])
        .current_dir(repo)
        .status()
        .unwrap();
    assert!(!status.success(), "expected a rebase conflict");
}

#[test]
fn restack_stages_resolver_fixed_files_before_continue() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);
    make_rebase_conflict(&repo);

    let mock_bin = tmp.path().join("bin");
    fs::create_dir(&mock_bin).unwrap();
    let mock_log = tmp.path().join("mock.log");
    make_executable(
        &mock_bin.join("claude"),
        r#"#!/bin/sh
echo "$PWD :: $*" >> "$MOCK_LOG"
printf 'resolved\n' > demo.txt
exit 0
"#,
    );

    let path = format!("{}:/usr/bin:/bin", mock_bin.display());
    let out = run_cw(
        &repo,
        &path,
        &[
            ("GIT_EDITOR", "true"),
            ("MOCK_LOG", mock_log.to_str().unwrap()),
        ],
        &["restack", "--resolver", "claude"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let unresolved = git_output(&repo, &["diff", "--name-only", "--diff-filter=U"]);
    assert!(unresolved.trim().is_empty(), "expected no unresolved files");
    let status = git_output(&repo, &["status", "--porcelain"]);
    assert!(
        status.trim().is_empty(),
        "expected clean status, got {status:?}"
    );
}

#[test]
fn restack_creates_workspace_when_pr_has_no_worktree() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("condor");
    init_repo(&repo);
    commit_file(&repo, "README.md", "root\n", "root");

    let mock_bin = tmp.path().join("bin");
    fs::create_dir(&mock_bin).unwrap();
    make_executable(
        &mock_bin.join("gh"),
        r#"#!/bin/sh
# Only handle `gh pr view 8641 ...` — emit the tsv our parser expects.
if [ "$1" = "pr" ] && [ "$2" = "view" ] && [ "$3" = "8641" ]; then
    printf 'OPEN\tfeat-ws\tdevelop\n'
    exit 0
fi
exit 1
"#,
    );
    make_executable(
        &mock_bin.join("gt"),
        r#"#!/bin/sh
exit 0
"#,
    );

    let path = format!("{}:/usr/bin:/bin", mock_bin.display());
    let out = run_cw(
        &repo,
        &path,
        &[("CW_WRAPPER", "1")],
        &["restack", "8641"],
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    let expected_ws = tmp.path().join("condor_1");
    assert!(
        expected_ws.is_dir(),
        "expected workspace at {} — stdout={stdout:?} stderr={stderr:?}",
        expected_ws.display()
    );
    assert!(
        stdout.contains("Found PR #8641 \u{2192} feat-ws"),
        "stdout missing PR announce: {stdout:?}"
    );
    // The ready banner is human output → stderr (stdout carries only the
    // CW wrapper records under CW_WRAPPER=1).
    assert!(
        stderr.contains("Workspace 1 ready!"),
        "stderr missing ready banner: {stderr:?}"
    );
}

#[test]
fn restack_keeps_files_unmerged_when_markers_remain() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);
    make_rebase_conflict(&repo);

    let mock_bin = tmp.path().join("bin");
    fs::create_dir(&mock_bin).unwrap();
    make_executable(
        &mock_bin.join("claude"),
        r#"#!/bin/sh
exit 0
"#,
    );

    let path = format!("{}:/usr/bin:/bin", mock_bin.display());
    let out = run_cw(
        &repo,
        &path,
        &[("GIT_EDITOR", "true")],
        &["restack", "--resolver", "claude"],
    );
    assert!(!out.status.success(), "expected cw restack to fail");

    let unresolved = git_output(&repo, &["diff", "--name-only", "--diff-filter=U"]);
    assert_eq!(unresolved.trim(), "demo.txt");
}
