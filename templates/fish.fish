# cw shell wrapper for fish. Install via:
#     cw shell-init fish | source
#
# Binary emits TAB-separated records on stdout when CW_WRAPPER=1; no eval,
# argv exec directly.

function cw
    set -l _out (env CW_WRAPPER=1 command cw $argv)
    set -l _rc $status
    if test $_rc -ne 0
        return $_rc
    end
    test -z "$_out"; and return 0
    set -l _close 0
    for _line in $_out
        set -l _parts (string split \t -- $_line)
        test "$_parts[1]" != "CW"; and continue
        set -l _kind $_parts[2]
        set -l _argv
        for _i in (seq 3 (count $_parts))
            # Unescape \\, \t, \n, \r
            set -l _v $_parts[$_i]
            set _v (string replace -a '\\t' \t -- $_v)
            set _v (string replace -a '\\n' \n -- $_v)
            set _v (string replace -a '\\r' \r -- $_v)
            set _v (string replace -a '\\\\' '\\' -- $_v)
            set -a _argv $_v
        end
        switch $_kind
            case CD
                cd $_argv[1]
            case TITLE
                printf '\033]0;%s\007' $_argv[1]
            case MSG
                echo $_argv[1] >&2
            case EXEC
                $_argv
            case EXEC_BG
                fish -c "$_argv" &
                disown
            case CLOSE_TAB
                set _close 1
        end
    end
    test $_close -eq 1; and kill -HUP %self 2>/dev/null
    return 0
end
