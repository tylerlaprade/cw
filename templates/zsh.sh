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
    local _out _rc _close=0
    _out=$(CW_WRAPPER=1 command cw "$@")
    _rc=$?
    (( _rc != 0 )) && return $_rc
    [[ -z "$_out" ]] && return 0
    local _line
    while IFS= read -r _line; do
        local -a _parts
        # Split on literal TAB; p-flag so \t in the separator string is parsed.
        _parts=("${(@ps:\t:)_line}")
        [[ "${_parts[1]}" != "CW" ]] && continue
        local _kind="${_parts[2]}"
        local -a _argv=()
        local _i
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
    done <<< "$_out"
    (( _close )) && kill -HUP $$ 2>/dev/null
    return 0
}
