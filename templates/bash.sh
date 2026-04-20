# cw shell wrapper for bash. Install via:
#     eval "$(cw shell-init bash)"
#
# Binary emits TAB-separated records (CW\tKIND\tfield\t…) on stdout when
# CW_WRAPPER=1. No eval; argv records exec directly via arrays.

cw() {
    local _out _rc _close=0
    _out=$(CW_WRAPPER=1 command cw "$@")
    _rc=$?
    (( _rc != 0 )) && return $_rc
    [[ -z "$_out" ]] && return 0
    local _line
    while IFS= read -r _line; do
        local -a _parts
        IFS=$'\t' read -r -a _parts <<< "$_line"
        [[ "${_parts[0]}" != "CW" ]] && { printf '%s\n' "$_line"; continue; }
        local _kind="${_parts[1]}"
        local -a _argv=()
        local _i
        for (( _i=2; _i < ${#_parts[@]}; _i++ )); do
            _argv+=("$(printf '%b' "${_parts[$_i]}")")
        done
        case "$_kind" in
            CD)        (( ${#_argv[@]} )) && builtin cd -- "${_argv[0]}" ;;
            TITLE)     printf '\033]0;%s\007' "${_argv[0]}" ;;
            MSG)       printf '%s\n' "${_argv[0]}" >&2 ;;
            EXEC)      "${_argv[@]}" </dev/tty ;;
            EXEC_BG)   { "${_argv[@]}" & } 2>/dev/null ; disown 2>/dev/null ;;
            CLOSE_TAB) _close=1 ;;
        esac
    done <<< "$_out"
    (( _close )) && kill -HUP $$ 2>/dev/null
    return 0
}
