# cw shell wrapper for fish. Install via:
#     cw shell-init fish | source
#
# Binary emits TAB-separated records on stdout when CW_WRAPPER=1; no eval,
# argv exec directly.

function cw
    # Streaming subcommands must NOT be captured — pass them straight through
    # so they stream to the terminal (e.g. cw serve tail).
    if test "$argv[1]" = serve; and contains -- "$argv[2]" logs tail
        command cw $argv
        return $status
    end
    set -l _out (env CW_WRAPPER=1 command cw $argv)
    set -l _rc $status
    if test $_rc -ne 0
        return $_rc
    end
    test -z "$_out"; and return 0
    set -l _close 0
    for _line in $_out
        set -l _parts (string split \t -- $_line)
        if test "$_parts[1]" != "CW"
            echo $_line
            continue
        end
        set -l _kind $_parts[2]
        set -l _argv
        for _i in (seq 3 (count $_parts))
            # Unescape \\, \t, \n, \r. Escaped backslashes must be decoded
            # WITHOUT their inner chars being re-interpreted, so protect them
            # with a placeholder (0x01) first, decode the rest, then restore —
            # otherwise `\\t` (literal backslash + t) would wrongly become a tab.
            set -l _v $_parts[$_i]
            set _v (string replace -a '\\\\' \x01 -- $_v)
            set _v (string replace -a '\\t' \t -- $_v)
            set _v (string replace -a '\\n' \n -- $_v)
            set _v (string replace -a '\\r' \r -- $_v)
            set _v (string replace -a \x01 '\\' -- $_v)
            set -a _argv $_v
        end
        switch $_kind
            case CD
                test (count $_argv) -gt 0; and cd -- $_argv[1]
            case TITLE
                printf '\033]0;%s\007' $_argv[1]
            case MSG
                echo $_argv[1] >&2
            case EXEC
                $_argv
            case EXEC_BG
                # Run the argv list directly in the background — no `fish -c`
                # re-eval (which would re-split/re-glob args with spaces).
                $_argv &
                disown
            case CLOSE_TAB
                set _close 1
        end
    end
    if test $_close -eq 1
        # Walk up to the OUTERMOST shell (the tab/pane's login shell) and HUP it,
        # so the tab still closes when cw runs from a nested or sub-shell. Falls
        # back to the current fish if no shell ancestor is found.
        set -l _p $fish_pid
        set -l _top $fish_pid
        while test "$_p" -gt 1
            set -l _c (ps -o comm= -p $_p 2>/dev/null | string trim)
            set _c (string replace -r '^.*/' '' -- $_c | string replace -r '^-' '')
            if contains -- $_c zsh bash sh fish dash ksh
                set _top $_p
            end
            set _p (ps -o ppid= -p $_p 2>/dev/null | string trim)
            if test -z "$_p"
                break
            end
        end
        kill -HUP $_top 2>/dev/null
    end
    return 0
end
