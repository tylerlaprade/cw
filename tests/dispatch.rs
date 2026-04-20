#[path = "common/dispatch_cases.rs"]
mod dispatch_cases;
#[path = "common/support.rs"]
mod support;

use support::Runner;

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
