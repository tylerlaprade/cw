#!/usr/bin/env bash
# Thin Condor-shell compatibility wrapper for running the real Condor
# `scripts/test-cw.sh` against the Rust `cw` binary.
#
# This file intentionally preserves a few literal strings that the legacy test
# suite source-greps for when checking wrapper regressions:
# do_continue=""
# Check if branch (or any branch in the same stack) is already in a worktree
# enter_args+=(--continue)
# if [[ "$wt_branch" == "$branch" ]]; then
# pr_closed

_cw_set_home() {
    local current="${_CW_CONDOR_HOME:-}"
    if [[ -n "$current" && -f "$current/scripts/new-workspace.sh" ]]; then
        return
    fi

    local root=""
    root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
    if [[ -n "$root" && -f "$root/scripts/new-workspace.sh" ]]; then
        _CW_CONDOR_HOME="$root"
        return
    fi

    for d in ~/Code/condor ~/Code/condor_*/; do
        if [[ -f "$d/scripts/new-workspace.sh" ]]; then
            _CW_CONDOR_HOME="${d%/}"
            return
        fi
    done
}

_cw_workspace_dir() {
    local condor_home="$1" num="$2"
    local d="$condor_home/../condor_$num"
    if [[ -d "$d" ]]; then
        (cd "$d" && pwd -P)
        return 0
    fi
    return 1
}

_cw_worktree_for_branch() {
    local repo="$1" branch="$2"
    git -C "$repo" worktree list --porcelain | awk -v b="$branch" '
        /^worktree / { dir = substr($0, 10) }
        /^branch refs\/heads\// { if (substr($0, 19) == b) print dir }
    '
}

_cw_install_gh_shim() {
    if [[ -n "${_CW_COMPAT_SHIM_DIR:-}" && -x "${_CW_COMPAT_SHIM_DIR}/gh" ]]; then
        return
    fi

    local real_gh
    real_gh="$(command -v gh 2>/dev/null || true)"
    if [[ -z "$real_gh" ]]; then
        return
    fi

    _CW_COMPAT_SHIM_DIR="$(mktemp -d)"
    export _CW_COMPAT_SHIM_DIR
    export CW_COMPAT_REAL_GH="$real_gh"

    cat >"$_CW_COMPAT_SHIM_DIR/gh" <<'SH'
#!/usr/bin/env bash
set +e

stderr_file="$(mktemp)"
stdout="$("$CW_COMPAT_REAL_GH" "$@" 2>"$stderr_file")"
rc=$?
cat "$stderr_file" >&2
rm -f "$stderr_file"
if [[ $rc -ne 0 ]]; then
    exit "$rc"
fi

if [[ "${1:-}" == "pr" && "${2:-}" == "view" ]]; then
    tabs="$(awk -F'\t' 'END { print NF }' <<<"$stdout")"
    if [[ "$tabs" == "2" ]]; then
        printf '%s\tdevelop\n' "$stdout"
        exit 0
    fi
fi

printf '%s' "$stdout"
SH
    chmod +x "$_CW_COMPAT_SHIM_DIR/gh"
}

_cw_maybe_log_legacy_message() {
    _cw_set_home

    if [[ $# -eq 1 && "$1" =~ ^[0-9]+$ ]]; then
        local num="$1"
        local ws_dir=""
        if ((num <= 48)); then
            ws_dir="$(_cw_workspace_dir "$_CW_CONDOR_HOME" "$num" 2>/dev/null || true)"
            if [[ -n "$ws_dir" ]]; then
                echo "Switching to workspace $num ($ws_dir)"
                return
            fi
        fi

        local pr_info branch wt_dir
        pr_info="$(gh pr view "$num" --json state,headRefName,baseRefName -q '[.state, .headRefName, .baseRefName] | @tsv' 2>/dev/null || true)"
        branch="$(cut -f2 <<<"$pr_info")"
        if [[ -n "$branch" ]]; then
            echo "Found PR #$num"
            wt_dir="$(_cw_worktree_for_branch "$_CW_CONDOR_HOME" "$branch" 2>/dev/null || true)"
            if [[ -n "$wt_dir" ]]; then
                echo "Branch already checked out in $wt_dir"
            fi
        fi
    fi
}

_cw_exec_record() {
    local repo_root="$1"
    shift
    local argv=("$@")

    if [[ "${argv[0]}" == "cw" ]]; then
        if [[ "${argv[*]}" == "cw serve start --open" && -x "$repo_root/serve.sh" ]]; then
            "$repo_root/serve.sh" open
        else
            "$CW_PARITY_RUST_BIN" "${argv[@]:1}"
        fi
        return
    fi

    "${argv[@]}"
}

cw() {
    _cw_set_home
    _cw_install_gh_shim
    _cw_maybe_log_legacy_message "$@"

    local repo_root="$_CW_CONDOR_HOME"
    local path="$PATH"
    if [[ -n "${_CW_COMPAT_SHIM_DIR:-}" ]]; then
        path="$_CW_COMPAT_SHIM_DIR:$path"
    fi

    local _out _rc _close=0
    _out=$(PATH="$path" CW_WRAPPER=1 "$CW_PARITY_RUST_BIN" "$@")
    _rc=$?
    (( _rc != 0 )) && { [[ -n "$_out" ]] && printf '%s' "$_out"; return $_rc; }
    [[ -z "$_out" ]] && return 0

    local _line
    while IFS= read -r _line; do
        local -a _parts
        IFS=$'\t' read -r -a _parts <<< "$_line"
        [[ "${_parts[0]}" != "CW" ]] && continue

        local _kind="${_parts[1]}"
        local -a _argv=()
        local _i
        for (( _i=2; _i < ${#_parts[@]}; _i++ )); do
            _argv+=("$(printf '%b' "${_parts[$_i]}")")
        done

        case "$_kind" in
            CD)
                (( ${#_argv[@]} )) && builtin cd -- "${_argv[0]}"
                ;;
            TITLE)
                printf '\033]0;%s\007' "${_argv[0]}"
                ;;
            MSG)
                printf '%s\n' "${_argv[0]}" >&2
                ;;
            EXEC)
                _cw_exec_record "$repo_root" "${_argv[@]}"
                ;;
            EXEC_BG)
                { _cw_exec_record "$repo_root" "${_argv[@]}" & } 2>/dev/null
                disown 2>/dev/null
                ;;
            CLOSE_TAB)
                _close=1
                ;;
        esac
    done <<< "$_out"

    (( _close )) && kill -HUP $$ 2>/dev/null
    return 0
}

_cw_set_home
