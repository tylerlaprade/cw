#!/usr/bin/env bash
set -uo pipefail

if [[ $# -lt 5 || "$5" != "--" ]]; then
    echo "usage: $0 <cwd> <events> <stdout> <stderr> -- <cw args...>" >&2
    exit 2
fi

start_dir="$1"
events="$2"
stdout_file="$3"
stderr_file="$4"
shift 5

normalize_path() {
    local path="$1"
    (cd "$path" && pwd -P)
}

(
    set +e +u
    cd "$start_dir" || exit 1
    # shellcheck source=/dev/null
    source "./scripts/cw.sh"

    _set_tab_title() { :; }
    direnv() { return 0; }
    claude() {
        printf 'EXEC\tclaude' >>"$events"
        for arg in "$@"; do
            printf '\t%s' "$arg" >>"$events"
        done
        printf '\n' >>"$events"
    }
    codex() {
        printf 'EXEC\tcodex' >>"$events"
        for arg in "$@"; do
            printf '\t%s' "$arg" >>"$events"
        done
        printf '\n' >>"$events"
    }

    cw "$@" >"$stdout_file" 2>"$stderr_file"
    rc=$?

    final_cwd="$(normalize_path "$PWD")"
    printf 'CWD\t%s\n' "$final_cwd" >>"$events"

    base="$(basename "$final_cwd")"
    if [[ "$base" =~ _([0-9]+)$ ]]; then
        printf 'TITLE\t#%s\n' "${BASH_REMATCH[1]}" >>"$events"
    fi

    exit "$rc"
)
exit $?
