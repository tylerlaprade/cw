# cw shell wrapper for zsh. Install via:
#     eval "$(cw shell-init zsh)"
#
# The wrapper runs the cw binary with CW_WRAPPER=1 and consumes TAB-separated
# records on stdout (CW\tKIND\tfield\t…). No eval; argv records are executed
# directly via shell arrays. Escape scheme: backslash, tab, newline are
# backslash-escaped in payload fields and unescaped here via printf %b.

cw() {
    emulate -L zsh
    setopt local_options no_aliases
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
    # Iterate via array split, NOT `while read <<< "$_out"` — the here-string
    # would pin the loop's stdin to the record payload, so any EXEC inside
    # would inherit that drained fd instead of the user's terminal. This
    # previously required `EXEC "${_argv[@]}" </dev/tty` as a workaround,
    # which broke TUIs (Bun/Ink in Claude Code) that expect fd 0 to be the
    # controlling terminal at process start, not a reopen of /dev/tty.
    local _line
    local -a _lines=("${(@f)_out}")
    for _line in "${_lines[@]}"; do
        local -a _parts
        # Split on literal TAB; p-flag so \t in the separator string is parsed.
        _parts=("${(@ps:\t:)_line}")
        [[ "${_parts[1]}" != "CW" ]] && { print -r -- "$_line"; continue; }
        local _kind="${_parts[2]}"
        local -a _argv=()
        local _i=0
        for (( _i=3; _i <= ${#_parts[@]}; _i++ )); do
            _argv+=("$(printf '%b' "${_parts[$_i]}")")
        done
        case "$_kind" in
            CD)        (( ${#_argv[@]} )) && builtin cd -- "${_argv[1]}" ;;
            TITLE)     printf '\033]0;%s\007' "${_argv[1]}" ;;
            MSG)       print -u2 -- "${_argv[1]}" ;;
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
