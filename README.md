# cw

Creates isolated Git workspaces with separate services, ports, and database
clones.

Works on any repo that uses the `{stem}_{N}` sibling-directory convention.
Configuration is largely autodetected; a `.devcli.toml` at repo root is
optional and only needed for things the tool can't safely infer (per-service
`pre_start` hooks, database clone config, repo-specific hooks, resolver
choice, etc.).

## Install

```sh
cargo install --path .
# in your shell init (zsh/bash):
eval "$(cw shell-init zsh)"          # or: eval "$(cw shell-init bash)"
# fish (different syntax — eval can't read a multi-line function):
cw shell-init fish | source
```

The wrapper installs exactly one shell function (`cw`). `cd`, terminal
title, and subprocess launches go through a `CW_WRAPPER=1` stdout contract
so the wrapper can change the caller's shell state. No `eval` — argv records
execute directly via shell arrays.

## Subcommands

```
cw <description>                         # create workspace, cd in, launch editor with prompt
cw <target>                              # cd into workspace; target = N | PR# | branch
cw -s <description>                      # stack on current branch (Graphite parent)
cw --base <branch> <description>         # branch a new workspace off an arbitrary base
cw --tmp <description>                   # ephemeral workspace in /tmp/{stem}_N
cw <target> --continue                   # cd + claude --continue
cw <target> --pr <N>                     # cd + claude --from-pr N

cw open [target]                         # start services + open browser
cw restack [target] [--resolver X] [--no-hook]    # rebase with optional hook + resolver
cw resolve <files...> [--resolver X]     # run the resolver on conflicted files (for hooks)
cw serve <start|stop|restart|status|logs|tail> [target] [--tail --open --no-ai -n N --service S]
cw remove <target...> [--force --dry-run --no-close-tab]
cw cleanup [--dry-run --force]
cw triage [--verbose]                    # actionable PRs + tickets

cw workspace list
cw workspace resolve <target> [--json]
cw workspace next-number                 # print lowest-available workspace number

cw init                                  # scaffold minimal .devcli.toml
cw shell-init <zsh|bash|fish>
cw completions <zsh|bash|fish>
cw config <show|validate>
cw doctor
```

Any `<target>` slot accepts a workspace number, a PR number, or a branch
name interchangeably. Numeric tokens prefer workspace-number resolution
and fall through to PR lookup when no matching worktree exists.

## What's autodetected

- **stem**: the basename of the main worktree, with a trailing `_N` stripped.
- **base branch**: `develop` → `main` → `master`, whichever ref exists.
- **services**: `manage.py` → Django backend (port `8000+N`); `package.json`
  with a `dev`/`start` script → JS frontend (port `3000+N`).
- **dep installers** (for background setup): `uv sync` when `uv.lock` +
  `pyproject.toml` are present; `bun install` with a `bun.lock`; else
  `npm install`.
- **env file copy**: `.env` + `.env.local` at the repo root and in every
  top-level subdir, unless `[env] copy = [...]` is set explicitly.
- **integrations**: `gt` → Graphite on; `gh` → GitHub on; `claude`/`codex`
  → available as resolvers; `direnv` → `.envrc` allowed on new worktrees.

## What needs config

A `.devcli.toml` is only needed when you want to:

- Cap workspace count (also the number-vs-PR threshold; default 99): `[workspace] max_count`
- Pre-start hooks per service (`[[services]]` with `pre_start` / `start_env`)
- Clone per-workspace databases: `[databases] pattern`, `suffixes`, `clone`,
  and a `post_clone` command (e.g. migrate the clone up to the branch's schema)
- Hand off restack customization to a repo-specific hook:
  `[restack] hook = "scripts/cw-restack-hook.sh"` (or drop one at that path
  and it's picked up automatically).
- Protect long-lived branches from the cleanup sweep + tune its inactivity
  window: `[cleanup] protected_branches`, `stale_hours` (the base branch is
  always protected; no branch names are hard-coded)
- Triage tuning: `[triage] jira_statuses`, `jira_project` (branch-independent
  ticket dashboard), `jira_site` (clickable Jira keys)
- Merge Claude Code memories across worktrees: `[claude] memory_merge`
- Post-create / pre-remove hooks: `[hooks]`
- Explicit env strip/inject rules: `[env]`

Run `cw init` for an interactive scaffolder, or `cw config show` to see
the effective autodetected config before writing anything.

## Wrapper record contract

The binary emits TAB-separated records to stdout when `CW_WRAPPER=1`:

```
CW<TAB>CD<TAB>/abs/path
CW<TAB>TITLE<TAB>#3
CW<TAB>MSG<TAB>Workspace 3 ready
CW<TAB>EXEC<TAB>claude<TAB>--from-pr<TAB>7543
CW<TAB>EXEC_BG<TAB>/path/script<TAB>--flag
CW<TAB>CLOSE_TAB<TAB>1
```

`\`, `\t`, `\n`, `\r` in fields are backslash-escaped by the binary and
unescaped in the wrapper via `printf %b`. Running `CW_WRAPPER=1 cw <args>`
without the wrapper prints exactly what would happen — inspectable.

## Restack

`cw restack` runs a generic loop — `gt r` or `git rebase {base}` — and on
each conflict:

1. Invokes `./scripts/cw-restack-hook.sh` (or the configured path) with the
   unresolved paths as argv. Hooks do arbitrary work: `makemigrations
   --merge`, `pytest --snapshot-update`, `bun run lint --fix`, staging
   anything they resolved.
2. For whatever the hook left unresolved, calls the chosen resolver:
   - `claude`: `claude -p ... --permission-mode acceptEdits`
   - `codex`: `codex exec ...`
   - `manual`: print file list, exit 0 — user resolves and re-runs.
3. `git add -u && (gt continue | git rebase --continue)`.
4. Loops.

Resume is free: kill mid-resolver and re-run `cw restack`; state is entirely
in git.

## License

MIT — see [LICENSE](LICENSE).
