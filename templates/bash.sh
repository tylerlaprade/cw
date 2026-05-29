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
    # Populate via a read loop, NOT mapfile (bash 4+, absent on macOS's system
    # bash 3.2 — mapfile there silently leaves _lines empty, dropping every
    # record). The here-string binds to THIS loop only, so the dispatch loop
    # below keeps the function's fd 0 for EXEC children.
    local -a _lines=()
    local _line
    while IFS= read -r _line; do
        _lines+=("$_line")
    done <<< "$_out"
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
    if (( _close )); then
        # Walk up to the OUTERMOST shell (the tab/pane's login shell) and HUP it,
        # so the tab still closes when cw runs from a nested or sub-shell. The
        # original (a child script) walked from $PPID; the wrapper IS the shell,
        # so it walks from $$. Falls back to $$ if no shell ancestor is found.
        local _p=$$ _top=$$ _c
        while [[ "$_p" -gt 1 ]]; do
            _c=$(ps -o comm= -p "$_p" 2>/dev/null)
            _c=${_c##*/}; _c=${_c#-}
            case "$_c" in zsh|bash|sh|fish|dash|ksh) _top=$_p ;; esac
            _p=$(ps -o ppid= -p "$_p" 2>/dev/null | tr -d ' ')
            [[ -z "$_p" ]] && break
        done
        kill -HUP "$_top" 2>/dev/null
    fi
    return 0
}
