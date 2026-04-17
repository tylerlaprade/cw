# cw

Numbered-workspace dev CLI: git worktrees, local services, rebase/restack,
workspace teardown, and an actionable-work dashboard.

Works on any repo that adopts the `{stem}_{N}` sibling-directory convention.
Configuration is largely autodetected; a `.devcli.toml` at repo root is
optional and only needed for things the tool can't safely infer.

## Install

```sh
cargo install --path .
# then, in your shell init (zsh):
eval "$(cw shell-init zsh)"
```

The wrapper installs a single `cw` function. `cd`, terminal title, and
subprocess launches go through a `CW_WRAPPER=1` stdout contract so the
wrapper can change the user's shell state. No `eval`; argv records exec
directly via shell arrays.

## Subcommands

```
cw <description>                         create workspace, launch editor, cd in
cw <target>                              cd into workspace; target = N | PR# | branch
cw open [target]                         start services, open browser
cw restack [target]                      rebase + auto-resolve
cw serve <start|stop|restart|status|logs|tail> [target]
cw remove <target...> [--force --dry-run --no-close-tab]
cw cleanup [--dry-run --force]
cw triage [--verbose]                    actionable PRs + tickets
cw workspace list
cw workspace resolve <target>
cw init                                  interactive .devcli.toml scaffolder
cw shell-init <zsh|bash|fish>            print shell wrapper source
cw doctor                                check PATH for required deps
```

## Status

Pre-0.1 scaffold. Subcommands are wired; implementations land incrementally
in steps 2–11 of the plan.
