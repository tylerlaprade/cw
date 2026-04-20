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

/// `cw <PR#> "<prompt>"` against an existing same-branch worktree must NOT
/// pass `--continue` to claude. Prompt = fresh session intent; --continue
/// would trigger claude's "No conversations found to resume" picker when no
/// prior session exists. See Condor commit f6413faa0.
pub fn pr_with_prompt_does_not_pass_continue(runner: Runner) {
    let tmp = TempDir::new().unwrap();
    // Bare origin so legacy cw.sh's `_gh_repo` + origin-fetch paths work.
    let origin = tmp.path().join("origin.git");
    crate::support::git(
        tmp.path(),
        &[
            "init",
            "--bare",
            "--initial-branch=develop",
            origin.to_str().unwrap(),
        ],
    );
    let repo = tmp.path().join("condor");
    init_repo(&repo);
    crate::support::git(&repo, &["remote", "add", "origin", origin.to_str().unwrap()]);
    commit_file(&repo, "README.md", "root\n", "root");
    crate::support::git(&repo, &["push", "-u", "origin", "develop", "--quiet"]);
    if matches!(runner, Runner::Legacy) {
        install_legacy_scripts(&repo);
    }

    // Same-branch worktree with at least one unique commit so
    // find_stack_worktree's fast path catches it in the legacy path.
    let ws = tmp.path().join("condor_for_pr");
    crate::support::git(
        &repo,
        &[
            "worktree",
            "add",
            ws.to_str().unwrap(),
            "-b",
            "pr-branch",
            "develop",
        ],
    );
    if matches!(runner, Runner::Legacy) {
        install_legacy_scripts(&ws);
    }
    crate::support::git(
        &ws,
        &["commit", "--allow-empty", "-m", "pr work", "--quiet"],
    );

    let mock_bin = tmp.path().join("bin");
    fs::create_dir(&mock_bin).unwrap();
    let claude_log = tmp.path().join("claude.log");
    crate::support::make_executable(
        &mock_bin.join("claude"),
        &format!(
            "#!/usr/bin/env bash\n{{ printf 'claude'; for a in \"$@\"; do printf '\\t%s' \"$a\"; done; printf '\\n'; }} >> {log}\nexit 0\n",
            log = claude_log.display()
        ),
    );
    // gh mock: honor whatever -q jq-path the caller asks for. Legacy uses
    // `[.state, .headRefName] | @tsv`; Rust's dispatcher uses its own gh path.
    crate::support::make_executable(
        &mock_bin.join("gh"),
        r#"#!/usr/bin/env bash
# gh pr view <num> ... -q '<jq>' ...
if [[ "$1" == "pr" && "$2" == "view" && "$3" == "8622" ]]; then
    # Parse the -q argument out of the remaining flags.
    jq=""
    shift 3
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -q|--jq) jq="$2"; shift 2 ;;
            *) shift ;;
        esac
    done
    case "$jq" in
        *state*headRefName*baseRefName*) printf 'OPEN\tpr-branch\tdevelop\n' ;;
        *state*headRefName*)             printf 'OPEN\tpr-branch\n' ;;
        *headRefName*)                   printf 'pr-branch\n' ;;
        *)                               printf '{"state":"OPEN","headRefName":"pr-branch","baseRefName":"develop"}\n' ;;
    esac
    exit 0
fi
if [[ "$1" == "pr" && "$2" == "list" ]]; then
    exit 0
fi
exit 1
"#,
    );
    crate::support::make_executable(
        &mock_bin.join("gt"),
        "#!/usr/bin/env bash\nexit 0\n",
    );
    let path = format!("{}:/usr/bin:/bin", mock_bin.display());

    let mut env: Vec<(&str, &str)> = Vec::new();
    let wrapper_env;
    let log_env;
    if matches!(runner, Runner::Rust) {
        wrapper_env = ("CW_WRAPPER", "1");
        env.push(wrapper_env);
    }
    let log_path = claude_log.display().to_string();
    log_env = ("MOCK_CLAUDE_LOG", log_path.as_str());
    env.push(log_env);

    let out = run_cw(runner, &repo, &path, &env, &["8622", "fix shellcheck error"]);
    assert!(
        out.status.success(),
        "cw 8622 <prompt> failed: {}",
        combined_output(&out)
    );

    let invocation = match runner {
        Runner::Rust => {
            // Grep the CW EXEC claude record emitted on stdout.
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            stdout
                .lines()
                .find(|l| l.starts_with("CW\tEXEC\tclaude"))
                .unwrap_or_else(|| {
                    panic!("no CW EXEC claude record on stdout:\n{stdout}")
                })
                .to_string()
        }
        Runner::Legacy => fs::read_to_string(&claude_log).unwrap_or_default(),
    };

    assert!(
        !invocation.contains("--continue"),
        "claude was invoked with --continue; prompt should have suppressed it:\n{invocation}"
    );
    assert!(
        invocation.contains("fix shellcheck error"),
        "claude invocation missing the prompt text:\n{invocation}"
    );
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
