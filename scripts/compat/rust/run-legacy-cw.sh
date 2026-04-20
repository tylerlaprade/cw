#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel 2>/dev/null || pwd -P)"
script_home="$root"

if [[ ! -f "$script_home/scripts/cw.sh" ]]; then
    echo "missing legacy cw.sh in $script_home/scripts" >&2
    exit 2
fi

stdout_file="$(mktemp)"
stderr_file="$(mktemp)"
trap 'rm -f "$stdout_file" "$stderr_file"' EXIT

# shellcheck source=/dev/null
source "$script_home/scripts/cw.sh"

_set_tab_title() { :; }
direnv() { return 0; }

set +e
cw "$@" >"$stdout_file" 2>"$stderr_file"
rc=$?
set -e

if [[ -n "${CW_WRAPPER:-}" ]]; then
    if [[ $rc -eq 0 ]]; then
        final_cwd="$(pwd -P)"
        printf 'CW\tCD\t%s\n' "$final_cwd"

        base="$(basename "$final_cwd")"
        if [[ "$base" =~ _([0-9]+)$ ]]; then
            printf 'CW\tTITLE\t#%s\n' "${BASH_REMATCH[1]}"
        fi

        if [[ "${1:-}" == "open" ]]; then
            printf 'CW\tEXEC\tcw\tserve\tstart\t--open\n'
        fi
    fi
else
    cat "$stdout_file"
fi

cat "$stderr_file" >&2
exit "$rc"
