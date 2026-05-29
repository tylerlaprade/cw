use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "cw",
    version,
    about = "Numbered-workspace dev-CLI",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Emit the shell-wrapper source. Install with `eval "$(cw shell-init
    /// zsh)"` (zsh/bash) or `cw shell-init fish | source` (fish).
    ShellInit {
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Check PATH for required + optional dependencies.
    Doctor,
    /// Config tools: show the effective merged config, etc.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Start/stop/status/logs for configured services in a workspace.
    Serve(ServeArgs),
    /// Start services + open the project in the browser.
    Open { target: Option<String> },
    /// Rebase + auto-resolve conflicts (optional hook + resolver).
    Restack(RestackArgs),
    /// Run the configured resolver on the given conflicted files.
    /// Intended for restack hooks that need the user's resolver without
    /// hardcoding a specific CLI.
    Resolve(ResolveArgs),
    /// Tear down one or more workspaces.
    Remove(RemoveArgs),
    /// Sweep stale workspaces + branches + orphaned DBs.
    Cleanup(CleanupArgs),
    /// Dashboard: actionable PRs + tickets.
    Triage(TriageArgs),
    /// Workspace inventory + machine-readable resolution.
    Workspace(WorkspaceArgs),
    /// Interactive scaffolder for .devcli.toml (minimum overrides).
    Init,
    /// Emit shell completions for the given shell.
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// Print the effective merged config (autodetect + optional .devcli.toml).
    Show,
    /// Validate .devcli.toml (if present) and exit nonzero on error.
    Validate,
}

#[derive(Args, Debug)]
pub struct ServeArgs {
    /// start, stop, restart, status, logs, tail
    pub action: String,
    /// N | PR# | branch — selects workspace (default: cwd-derived).
    pub target: Option<String>,
    /// Follow (tail -f) the log stream. Applicable to start/logs.
    #[arg(long)]
    pub tail: bool,
    /// Open the project URL in the browser after start.
    #[arg(long)]
    pub open: bool,
    /// Skip AI-mode pre-start hook.
    #[arg(long)]
    pub no_ai: bool,
    /// Number of log lines for logs/tail.
    #[arg(short = 'n', long)]
    pub lines: Option<usize>,
    /// Service name filter (defaults to all configured).
    #[arg(long)]
    pub service: Option<String>,
}

#[derive(Args, Debug)]
pub struct RestackArgs {
    /// N | PR# | branch (default: cwd-derived workspace).
    pub target: Option<String>,
    /// Override the resolver (claude | codex | manual).
    #[arg(long)]
    pub resolver: Option<String>,
    /// Skip the repo hook script if present.
    #[arg(long)]
    pub no_hook: bool,
}

#[derive(Args, Debug)]
pub struct ResolveArgs {
    /// Conflicted files to hand to the configured resolver.
    pub files: Vec<String>,
    /// Override the resolver (claude | codex | manual).
    #[arg(long)]
    pub resolver: Option<String>,
}

#[derive(Args, Debug)]
pub struct RemoveArgs {
    /// N | PR# | branch — accepts multiple.
    pub targets: Vec<String>,
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub no_close_tab: bool,
}

#[derive(Args, Debug)]
pub struct CleanupArgs {
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct TriageArgs {
    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(Args, Debug)]
pub struct WorkspaceArgs {
    #[command(subcommand)]
    pub action: WorkspaceAction,
}

#[derive(Subcommand, Debug)]
pub enum WorkspaceAction {
    /// List all workspaces with status.
    List,
    /// Machine-readable resolution: target -> {dir, branch, n}.
    Resolve {
        target: String,
        #[arg(long)]
        json: bool,
    },
    /// Print the lowest-available workspace number (dry-run claim).
    NextNumber,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
}
