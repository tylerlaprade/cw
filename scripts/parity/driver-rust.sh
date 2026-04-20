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

bin="${CW_PARITY_RUST_BIN:-}"
if [[ -z "$bin" ]]; then
    echo "CW_PARITY_RUST_BIN is required" >&2
    exit 2
fi

normalize_path() {
    local path="$1"
    if [[ -d "$path" ]]; then
        (cd "$path" && pwd -P)
    else
        printf '%s\n' "$path"
    fi
}

unescape_field() {
    printf '%b' "$1"
}

final_cwd="$(normalize_path "$start_dir")"

(
    cd "$start_dir" || exit 1
    CW_WRAPPER=1 "$bin" "$@" >"$stdout_file" 2>"$stderr_file"
)
rc=$?

while IFS=$'\t' read -r -a parts; do
    [[ "${parts[0]:-}" == "CW" ]] || continue
    kind="${parts[1]:-}"
    case "$kind" in
    CD)
        final_cwd="$(normalize_path "$(unescape_field "${parts[2]:-}")")"
        ;;
    TITLE)
        printf 'TITLE\t%s\n' "$(unescape_field "${parts[2]:-}")" >>"$events"
        ;;
    EXEC | EXEC_BG)
        printf '%s' "$kind" >>"$events"
        for ((i = 2; i < ${#parts[@]}; i++)); do
            printf '\t%s' "$(unescape_field "${parts[$i]}")" >>"$events"
        done
        printf '\n' >>"$events"
        ;;
    MSG)
        printf 'MSG\t%s\n' "$(unescape_field "${parts[2]:-}")" >>"$events"
        ;;
    esac
done <"$stdout_file"

printf 'CWD\t%s\n' "$final_cwd" >>"$events"
exit "$rc"
