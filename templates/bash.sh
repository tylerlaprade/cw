# cw shell wrapper for bash. Install via:
#     eval "$(cw shell-init bash)"
#
# Binary emits TAB-separated records (CW\tKIND\tfield\t…) on stdout when
# CW_WRAPPER=1. No eval; argv records exec directly via arrays.

cw() {
    # Streaming subcommands must NOT be captured in $() — that buffers forever
    # and shows nothing. Pass them straight through so they stream to the tty.
    if [[ "$1" == "serve" && ( "$2" == "logs" || "$2" == "tail" ) ]]; then
        command cw "$@"
        return $?
    fi
    local _out _rc _close=0
    _out=$(CW_WRAPPER=1 command cw "$@")
    _rc=$?
    (( _rc != 0 )) && return $_rc
    [[ -z "$_out" ]] && return 0
    # Iterate via mapfile, NOT `while read <<< "$_out"` — a here-string loop
    # binds its stdin to the payload, leaving EXEC children with a drained
    # fd 0. The old fix was `EXEC … </dev/tty`, which broke Bun/Ink TUIs
    # that snapshot stdin at startup.
    local -a _lines=()
    mapfile -t _lines <<< "$_out"
    local _line
    for _line in "${_lines[@]}"; do
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
            EXEC)      "${_argv[@]}" ;;
            EXEC_BG)   { "${_argv[@]}" & } 2>/dev/null ; disown 2>/dev/null ;;
            CLOSE_TAB) _close=1 ;;
        esac
    done
    (( _close )) && kill -HUP $$ 2>/dev/null
    return 0
}
