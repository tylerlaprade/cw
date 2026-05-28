use crate::support::{
    add_worktree, combined_output, commit_file, init_repo, install_legacy_scripts, run_cw,
    Runner,
};
use std::fs;
use tempfile::TempDir;

pub fn open_target_emits_cd_title_and_exec_records(runner: Runner) {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("condor");
    init_repo(&repo);
    commit_file(&repo, "README.md", "root\n", "root");
    if matches!(runner, Runner::Legacy) {
        install_legacy_scripts(&repo);
    }

    let ws = tmp.path().join("condor_11");
    add_worktree(&repo, &ws, "br-11", runner);

    let out = run_cw(
        runner,
        &repo,
        "/usr/bin:/bin",
        &[("CW_WRAPPER", "1")],
        &["open", "11"],
    );
    assert!(out.status.success(), "{}", combined_output(&out));

    let canonical_ws = fs::canonicalize(&ws).unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(&format!("CW\tCD\t{}", canonical_ws.display())));
    assert!(stdout.contains("CW\tTITLE\t#11"));
    assert!(stdout.contains("CW\tEXEC\tcw\tserve\tstart\t--open"));
}

pub fn open_without_args_uses_current_workspace(runner: Runner) {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("condor");
    init_repo(&repo);
    commit_file(&repo, "README.md", "root\n", "root");
    if matches!(runner, Runner::Legacy) {
        install_legacy_scripts(&repo);
    }

    let ws = tmp.path().join("condor_12");
    add_worktree(&repo, &ws, "br-12", runner);

    let out = run_cw(
        runner,
        &ws,
        "/usr/bin:/bin",
        &[("CW_WRAPPER", "1")],
        &["open"],
    );
    assert!(out.status.success(), "{}", combined_output(&out));

    let canonical_ws = fs::canonicalize(&ws).unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(&format!("CW\tCD\t{}", canonical_ws.display())));
    assert!(stdout.contains("CW\tTITLE\t#12"));
    assert!(stdout.contains("CW\tEXEC\tcw\tserve\tstart\t--open"));
}

pub fn description_create_launches_claude_with_prompt(runner: Runner) {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("cwdisptest");
    init_repo(&repo);
    commit_file(&repo, "README.md", "root\n", "root");
    if matches!(runner, Runner::Legacy) {
        install_legacy_scripts(&repo);
    }

    // PATH without gt/gh so create stays git-only (no Graphite, no PR lookup).
    let out = run_cw(
        runner,
        &repo,
        "/usr/bin:/bin",
        &[("CW_WRAPPER", "1")],
        &["fix", "the", "bug"],
    );
    assert!(out.status.success(), "{}", combined_output(&out));

    let stdout = String::from_utf8_lossy(&out.stdout);
    // First-entry description-create: claude launched with the WHOLE description
    // as the prompt — no --continue (first entry), no --from-pr (no PR).
    assert!(
        stdout.contains("CW\tEXEC\tclaude\tfix the bug"),
        "expected claude launch with the description prompt.\nstdout: {stdout}"
    );
    assert!(
        !stdout.contains("--continue") && !stdout.contains("--from-pr"),
        "first-entry create must not --continue/--from-pr.\nstdout: {stdout}"
    );
    // The slugified branch's worktree was created as the lowest workspace.
    let ws = tmp.path().join("cwdisptest_1");
    assert!(ws.is_dir(), "expected workspace dir {}", ws.display());
}

pub fn numeric_token_without_workspace_or_pr_errors_instead_of_creating_branch(runner: Runner) {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("cw");
    init_repo(&repo);
    commit_file(&repo, "README.md", "root\n", "root");
    if matches!(runner, Runner::Legacy) {
        install_legacy_scripts(&repo);
    }

    let mock_bin = tmp.path().join("bin");
    fs::create_dir(&mock_bin).unwrap();
    crate::support::make_executable(
        &mock_bin.join("gh"),
        r#"#!/bin/sh
exit 1
"#,
    );
    let path = format!("{}:/usr/bin:/bin", mock_bin.display());

    let out = run_cw(runner, &repo, &path, &[], &["8622"]);
    assert!(!out.status.success(), "expected failure");
    let combined = combined_output(&out);
    assert!(combined.contains("8622"), "{combined}");

    let stem = repo.file_name().unwrap().to_string_lossy();
    let sibling = repo.parent().unwrap().join(format!("{}_1", stem));
    assert!(
        !sibling.exists(),
        "unexpected workspace created at {}",
        sibling.display()
    );
}
