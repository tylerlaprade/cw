#!/usr/bin/env bash
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

MODE="${1:-both}"

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

_pass() {
    TESTS_PASSED=$((TESTS_PASSED + 1))
    printf 'ok  %s\n' "$1"
}

_fail() {
    TESTS_FAILED=$((TESTS_FAILED + 1))
    printf 'not ok  %s\n' "$1"
    shift
    while [[ $# -gt 0 ]]; do
        printf '  %s\n' "$1"
        shift
    done
}

assert_equals() {
    TESTS_RUN=$((TESTS_RUN + 1))
    local expected="$1" actual="$2" name="$3"
    if [[ "$expected" == "$actual" ]]; then
        _pass "$name"
    else
        _fail "$name" "expected: $expected" "actual:   $actual"
    fi
}

assert_rc() {
    TESTS_RUN=$((TESTS_RUN + 1))
    local expected="$1" actual="$2" name="$3"
    if [[ "$expected" == "$actual" ]]; then
        _pass "$name"
    else
        _fail "$name" "expected rc: $expected" "actual rc:   $actual"
    fi
}

assert_contains_file() {
    TESTS_RUN=$((TESTS_RUN + 1))
    local file="$1" needle="$2" name="$3"
    if grep -Fq -- "$needle" "$file"; then
        _pass "$name"
    else
        _fail "$name" "missing: $needle" "file: $file" "contents: $(cat "$file" 2>/dev/null)"
    fi
}

assert_not_branch() {
    TESTS_RUN=$((TESTS_RUN + 1))
    local repo="$1" branch="$2" name="$3"
    if git -C "$repo" show-ref --verify --quiet "refs/heads/$branch"; then
        _fail "$name" "unexpected branch created: $branch"
    else
        _pass "$name"
    fi
}

normalize_path() {
    local path="$1"
    (cd "$path" && pwd -P)
}

last_event_value() {
    local kind="$1" file="$2"
    awk -F'\t' -v key="$kind" '$1 == key { value = $2 } END { print value }' "$file"
}

install_executable() {
    local path="$1" body="$2"
    printf '%s' "$body" >"$path"
    chmod +x "$path"
}

install_legacy_scripts() {
    local dir="$1"
    mkdir -p "$dir/scripts"
    cp "$CW_PARITY_LEGACY_ROOT/scripts/cw.sh" "$dir/scripts/cw.sh"
    cp "$CW_PARITY_LEGACY_ROOT/scripts/worktree-lib.sh" "$dir/scripts/worktree-lib.sh"
}

create_workspace() {
    local repo="$1" root="$2" number="$3" branch="$4"
    local ws="$root/condor_$number"
    git -C "$repo" worktree add "$ws" -b "$branch" develop >/dev/null 2>&1
    install_executable "$ws/restack.sh" '#!/usr/bin/env bash
printf "RESTACK\t%s\n" "$(pwd -P)" >>"$CW_PARITY_EVENTS"
'
    if [[ "$PARITY_IMPL" == "legacy" ]]; then
        install_legacy_scripts "$ws"
    fi
}

setup_case_repo() {
    local root="$1"
    CASE_ROOT="$root"
    REPO="$CASE_ROOT/condor"
    ORIGIN="$CASE_ROOT/origin.git"
    BIN_DIR="$CASE_ROOT/bin"
    MOCK_LOG="$CASE_ROOT/mock.log"

    mkdir -p "$REPO/scripts" "$BIN_DIR"
    : >"$MOCK_LOG"

    git init --bare --initial-branch=develop "$ORIGIN" >/dev/null 2>&1
    git init --initial-branch=develop "$REPO" >/dev/null 2>&1
    git -C "$REPO" config user.email "test@test.local"
    git -C "$REPO" config user.name "Test"
    git -C "$REPO" config commit.gpgsign false
    git -C "$REPO" remote add origin "$ORIGIN"
    : >"$REPO/README"
    git -C "$REPO" add README
    git -C "$REPO" commit -m root --quiet
    git -C "$REPO" push -u origin develop --quiet >/dev/null 2>&1 || true

    install_executable "$BIN_DIR/gh" '#!/usr/bin/env bash
if [[ "${1:-}" == "pr" && "${2:-}" == "view" ]]; then
    exit 1
fi
if [[ "${1:-}" == "pr" && "${2:-}" == "list" ]]; then
    exit 0
fi
exit 0
'

    install_executable "$BIN_DIR/gt" '#!/usr/bin/env bash
printf "%s :: %s\n" "$PWD" "$*" >>"$MOCK_LOG"
exit 0
'

    install_executable "$REPO/serve.sh" '#!/usr/bin/env bash
printf "EXEC\tcw\tserve\tstart\t--open\n" >>"$CW_PARITY_EVENTS"
'

    install_executable "$REPO/scripts/new-workspace.sh" '#!/usr/bin/env bash
exit 0
'

    if [[ "$PARITY_IMPL" == "legacy" ]]; then
        install_legacy_scripts "$REPO"
    fi

    create_workspace "$REPO" "$CASE_ROOT" 5 br-5
    create_workspace "$REPO" "$CASE_ROOT" 11 br-11
    create_workspace "$REPO" "$CASE_ROOT" 12 br-12

    export MOCK_LOG
}

run_case() {
    local name="$1" start_dir="$2"
    shift 2

    LAST_EVENTS="$CASE_ROOT/$name.events"
    LAST_STDOUT="$CASE_ROOT/$name.stdout"
    LAST_STDERR="$CASE_ROOT/$name.stderr"
    : >"$LAST_EVENTS"
    : >"$LAST_STDOUT"
    : >"$LAST_STDERR"

    export CW_PARITY_EVENTS="$LAST_EVENTS"
    PATH="$BIN_DIR:/usr/bin:/bin" "$DRIVER" "$start_dir" "$LAST_EVENTS" "$LAST_STDOUT" "$LAST_STDERR" -- "$@"
    LAST_RC=$?
}

case_workspace_switch() {
    setup_case_repo "$(mktemp -d)"
    run_case "workspace-switch" "$REPO" 5
    assert_rc 0 "$LAST_RC" "$PARITY_IMPL: cw 5 succeeds"
    assert_equals "$(normalize_path "$CASE_ROOT/condor_5")" "$(last_event_value CWD "$LAST_EVENTS")" "$PARITY_IMPL: cw 5 changes cwd"
    assert_contains_file "$LAST_EVENTS" $'TITLE\t#5' "$PARITY_IMPL: cw 5 emits workspace title"
}

case_missing_numeric_errors() {
    setup_case_repo "$(mktemp -d)"
    run_case "missing-numeric" "$REPO" 8622
    assert_rc 1 "$LAST_RC" "$PARITY_IMPL: cw 8622 errors"
    assert_contains_file "$LAST_STDERR" "8622" "$PARITY_IMPL: cw 8622 explains the failure"
    assert_not_branch "$REPO" "8622" "$PARITY_IMPL: cw 8622 does not create a branch"
}

case_open_target() {
    setup_case_repo "$(mktemp -d)"
    run_case "open-target" "$REPO" open 12
    assert_rc 0 "$LAST_RC" "$PARITY_IMPL: cw open 12 succeeds"
    assert_equals "$(normalize_path "$CASE_ROOT/condor_12")" "$(last_event_value CWD "$LAST_EVENTS")" "$PARITY_IMPL: cw open 12 changes cwd"
    assert_contains_file "$LAST_EVENTS" $'TITLE\t#12' "$PARITY_IMPL: cw open 12 emits workspace title"
    assert_contains_file "$LAST_EVENTS" $'EXEC\tcw\tserve\tstart\t--open' "$PARITY_IMPL: cw open 12 requests serve open"
}

case_open_current_workspace() {
    setup_case_repo "$(mktemp -d)"
    run_case "open-current" "$CASE_ROOT/condor_12" open
    assert_rc 0 "$LAST_RC" "$PARITY_IMPL: cw open succeeds in the current workspace"
    assert_equals "$(normalize_path "$CASE_ROOT/condor_12")" "$(last_event_value CWD "$LAST_EVENTS")" "$PARITY_IMPL: cw open keeps cwd in the current workspace"
    assert_contains_file "$LAST_EVENTS" $'TITLE\t#12' "$PARITY_IMPL: cw open emits current workspace title"
    assert_contains_file "$LAST_EVENTS" $'EXEC\tcw\tserve\tstart\t--open' "$PARITY_IMPL: cw open requests serve open"
}

case_restack_target() {
    setup_case_repo "$(mktemp -d)"
    run_case "restack-target" "$REPO" restack 11
    assert_rc 0 "$LAST_RC" "$PARITY_IMPL: cw restack 11 succeeds"
    assert_equals "$(normalize_path "$CASE_ROOT/condor_11")" "$(last_event_value CWD "$LAST_EVENTS")" "$PARITY_IMPL: cw restack 11 changes cwd"
    assert_contains_file "$LAST_EVENTS" $'TITLE\t#11' "$PARITY_IMPL: cw restack 11 emits workspace title"
}

assert_not_contains_file() {
    TESTS_RUN=$((TESTS_RUN + 1))
    local file="$1" needle="$2" name="$3"
    if grep -Fq -- "$needle" "$file"; then
        _fail "$name" "unexpected match: $needle" "file: $file" "contents: $(cat "$file" 2>/dev/null)"
    else
        _pass "$name"
    fi
}

case_zero_enters_repo_root() {
    setup_case_repo "$(mktemp -d)"
    run_case "zero-enter" "$CASE_ROOT/condor_11" 0
    assert_rc 0 "$LAST_RC" "$PARITY_IMPL: cw 0 succeeds"
    assert_equals "$(normalize_path "$REPO")" "$(last_event_value CWD "$LAST_EVENTS")" "$PARITY_IMPL: cw 0 changes cwd to repo root"
    assert_not_contains_file "$LAST_EVENTS" $'TITLE\t#0' "$PARITY_IMPL: cw 0 does not emit a workspace title"
}

case_open_zero() {
    setup_case_repo "$(mktemp -d)"
    run_case "open-zero" "$CASE_ROOT/condor_11" open 0
    assert_rc 0 "$LAST_RC" "$PARITY_IMPL: cw open 0 succeeds"
    assert_equals "$(normalize_path "$REPO")" "$(last_event_value CWD "$LAST_EVENTS")" "$PARITY_IMPL: cw open 0 changes cwd to repo root"
    assert_not_contains_file "$LAST_EVENTS" $'TITLE\t#0' "$PARITY_IMPL: cw open 0 does not emit a workspace title"
    assert_contains_file "$LAST_EVENTS" $'EXEC\tcw\tserve\tstart\t--open' "$PARITY_IMPL: cw open 0 requests serve open"
}

case_restack_zero() {
    setup_case_repo "$(mktemp -d)"
    # Install a restack.sh at the repo root so the legacy restack path has a
    # target to invoke. The parity harness already installs per-workspace ones.
    install_executable "$REPO/restack.sh" '#!/usr/bin/env bash
printf "RESTACK\t%s\n" "$(pwd -P)" >>"$CW_PARITY_EVENTS"
'
    run_case "restack-zero" "$CASE_ROOT/condor_11" restack 0
    assert_rc 0 "$LAST_RC" "$PARITY_IMPL: cw restack 0 succeeds"
    assert_equals "$(normalize_path "$REPO")" "$(last_event_value CWD "$LAST_EVENTS")" "$PARITY_IMPL: cw restack 0 changes cwd to repo root"
    assert_not_contains_file "$LAST_EVENTS" $'TITLE\t#0' "$PARITY_IMPL: cw restack 0 does not emit a workspace title"
}

run_mode() {
    PARITY_IMPL="$1"
    export PARITY_IMPL
    printf '== %s parity ==\n' "$PARITY_IMPL"

    case_workspace_switch
    case_missing_numeric_errors
    case_open_target
    case_open_current_workspace
    case_restack_target

    # `cw 0` → repo root is a cw (Rust) feature; the legacy cw.sh has no such
    # dispatch (numeric falls through to the PR path), so these cases describe
    # the Rust-only contract and must not be asserted against legacy.
    if [[ "$PARITY_IMPL" == "rust" ]]; then
        case_zero_enters_repo_root
        case_open_zero
        case_restack_zero
    fi
}

case "$MODE" in
rust)
    DRIVER="$REPO_ROOT/scripts/parity/driver-rust.sh"
    if [[ -z "${CW_PARITY_RUST_BIN:-}" ]]; then
        cargo build --quiet --bin cw
        export CW_PARITY_RUST_BIN="$REPO_ROOT/target/debug/cw"
    fi
    run_mode rust
    ;;
legacy)
    DRIVER="$REPO_ROOT/scripts/parity/driver-legacy.sh"
    : "${CW_PARITY_LEGACY_ROOT:?set CW_PARITY_LEGACY_ROOT to the legacy Bash repo root}"
    run_mode legacy
    ;;
both)
    DRIVER="$REPO_ROOT/scripts/parity/driver-rust.sh"
    if [[ -z "${CW_PARITY_RUST_BIN:-}" ]]; then
        cargo build --quiet --bin cw
        export CW_PARITY_RUST_BIN="$REPO_ROOT/target/debug/cw"
    fi
    run_mode rust
    DRIVER="$REPO_ROOT/scripts/parity/driver-legacy.sh"
    : "${CW_PARITY_LEGACY_ROOT:?set CW_PARITY_LEGACY_ROOT to the legacy Bash repo root}"
    run_mode legacy
    ;;
*)
    echo "usage: $0 [rust|legacy|both]" >&2
    exit 2
    ;;
esac

printf '\nTests run: %s\n' "$TESTS_RUN"
printf 'Passed:    %s\n' "$TESTS_PASSED"

if [[ "$TESTS_FAILED" -ne 0 ]]; then
    printf 'Failed:    %s\n' "$TESTS_FAILED"
    exit 1
fi
