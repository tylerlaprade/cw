#[path = "common/dispatch_cases.rs"]
mod dispatch_cases;
#[path = "common/support.rs"]
mod support;

use support::Runner;

fn skip_without_legacy() -> bool {
    if support::legacy_root().is_some() {
        false
    } else {
        eprintln!("skipping legacy dispatch cross-check: set CW_PARITY_LEGACY_ROOT to enable");
        true
    }
}

#[test]
fn open_target_emits_cd_title_and_exec_records() {
    if skip_without_legacy() {
        return;
    }
    dispatch_cases::open_target_emits_cd_title_and_exec_records(Runner::Legacy);
}

#[test]
fn open_without_args_uses_current_workspace() {
    if skip_without_legacy() {
        return;
    }
    dispatch_cases::open_without_args_uses_current_workspace(Runner::Legacy);
}

#[test]
fn numeric_token_without_workspace_or_pr_errors_instead_of_creating_branch() {
    if skip_without_legacy() {
        return;
    }
    dispatch_cases::numeric_token_without_workspace_or_pr_errors_instead_of_creating_branch(
        Runner::Legacy,
    );
}
