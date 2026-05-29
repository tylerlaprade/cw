use serde::{Deserialize, Serialize};

/// The effective (possibly-autodetected) config. Everything is optional at the
/// file level; defaults are filled in by the autodetect pass.
///
/// `deny_unknown_fields` on every struct makes a typo'd section or key a hard
/// parse error (so `cw config validate` reports it) rather than silently
/// ignored.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub workspace: WorkspaceCfg,
    #[serde(default)]
    pub integrations: Integrations,
    #[serde(default)]
    pub services: Vec<ServiceCfg>,
    #[serde(default)]
    pub deps: Option<DepsCfg>,
    #[serde(default)]
    pub databases: Option<DatabasesCfg>,
    #[serde(default)]
    pub restack: RestackCfg,
    #[serde(default)]
    pub hooks: HooksCfg,
    #[serde(default)]
    pub env: EnvCfg,
    #[serde(default)]
    pub cleanup: CleanupCfg,
    #[serde(default)]
    pub triage: TriageCfg,
    #[serde(default)]
    pub claude: ClaudeCfg,

    /// Computed at runtime, not read from file.
    #[serde(skip)]
    pub runtime: Runtime,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCfg {
    /// Highest workspace number to allocate / treat a numeric token as a
    /// workspace (above it, a number is resolved as a PR). Default 99 when
    /// unset — this generalizes the original tool's hard-coded 48 (an
    /// Auth0-callback limit), so set it to match any upstream cap you have.
    pub max_count: Option<u32>,
    /// Override auto-detected base branch (default: develop|main|master).
    pub base_branch: Option<String>,
    /// Override auto-detected stem (default: repo-root basename).
    pub stem: Option<String>,
    /// Background-restack a workspace onto base every time you re-enter it.
    /// Off by default: it rewrites local history in the background, which can
    /// surprise (force-push needed afterward). Opt in with `auto_restack = true`.
    #[serde(default)]
    pub auto_restack: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Integrations {
    pub graphite: Option<bool>,
    pub github: Option<bool>,
    pub claude: Option<bool>,
    pub codex: Option<bool>,
    pub direnv: Option<bool>,
    pub acli: Option<bool>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceCfg {
    pub name: String,
    #[serde(default)]
    pub alias: Vec<String>,
    /// Subdir under repo root (e.g. "server"). "." for repo root.
    pub subdir: Option<String>,
    pub port: Option<PortCfg>,
    pub start: Option<String>,
    #[serde(default)]
    pub start_env: std::collections::BTreeMap<String, String>,
    pub venv: Option<String>,
    pub pid_file: Option<String>,
    pub log_file: Option<String>,
    #[serde(default)]
    pub stop_patterns: Vec<String>,
    pub pre_start: Option<String>,
    pub open_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortCfg {
    pub base: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepsCfg {
    #[serde(default = "yes")]
    pub parallel: bool,
    pub install: Vec<DepInstall>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepInstall {
    pub dir: String,
    pub cmd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabasesCfg {
    /// e.g. "app_{n}_{suffix}"
    pub pattern: String,
    /// Suffixes to clone from the source workspace into the new workspace.
    /// Each destination suffix uses the source workspace's matching suffix.
    pub suffixes: Vec<String>,
    /// "postgres" or "none"
    #[serde(default = "default_clone")]
    pub clone: String,
    #[serde(default = "default_src_suffix")]
    pub default_source_suffix: String,
    /// Shell command run once after the clone completes, in the new workspace
    /// root, in the same detached setup phase (so a cloned DB can be migrated
    /// up to the branch's schema before first use). `{n}`/`{stem}` substituted.
    pub post_clone: Option<String>,
}

fn default_clone() -> String {
    "postgres".into()
}
fn default_src_suffix() -> String {
    "qa".into()
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestackCfg {
    pub hook: Option<String>,
    /// "claude" | "codex" | "manual"
    pub resolver: Option<String>,
    /// Submit the stack (`gt ss`) after a successful restack. Off by default —
    /// it pushes branches and opens/updates PRs and needs Graphite auth.
    #[serde(default)]
    pub submit: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HooksCfg {
    pub post_create: Option<String>,
    pub pre_remove: Option<String>,
    pub post_cd: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvCfg {
    /// Files to copy verbatim from source worktree to new worktree.
    #[serde(default)]
    pub copy: Vec<String>,
    /// Per-file strip rules: remove matching lines (regex) after copy.
    #[serde(default)]
    pub strip: Vec<EnvStrip>,
    /// Per-file line injection (with {n}, {stem}, {port} substitution).
    #[serde(default)]
    pub inject: Vec<EnvInject>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvStrip {
    pub file: String,
    pub patterns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvInject {
    pub file: String,
    pub line: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupCfg {
    /// Extra branches `cw cleanup` must never delete, beyond the configured
    /// base. Replaces the original tool's hard-coded `develop main release/qa
    /// release/staging`. Exact names; long-lived release/integration branches
    /// belong here so a merged-looking one isn't pruned.
    #[serde(default)]
    pub protected_branches: Vec<String>,
    /// Hours of inactivity (last-commit age) before a workspace with an
    /// open/draft PR is eligible for cleanup. Default 48 when unset.
    pub stale_hours: Option<u64>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeCfg {
    /// Merge Claude Code memories across this repo's worktrees: seed a new
    /// workspace with the union of sibling worktrees' MEMORY.md on create, and
    /// salvage a departing workspace's memories into the survivors before
    /// teardown. Off by default — it reads/writes `~/.claude/projects/`.
    #[serde(default)]
    pub memory_merge: bool,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriageCfg {
    /// Jira workflow statuses to surface as actionable. Default
    /// `["To Do", "In Progress"]` (Jira built-ins) when unset — the original
    /// hard-coded a company-custom `["Failed QA", "To Do"]`.
    #[serde(default)]
    pub jira_statuses: Vec<String>,
    /// Default Jira project key for the ticket dashboard, used when the current
    /// branch/PR has no inferable key (so `cw triage` works between branches).
    pub jira_project: Option<String>,
    /// Atlassian site host (e.g. "acme.atlassian.net") for clickable Jira keys.
    pub jira_site: Option<String>,
}

/// Values populated by the loader, not from the file.
#[derive(Debug, Default, Clone)]
pub struct Runtime {
    /// Absolute path to the discovered repo root (common_git_dir's parent).
    pub repo_root: Option<std::path::PathBuf>,
    /// Absolute path to the loaded .devcli.toml, if one was found.
    pub config_path: Option<std::path::PathBuf>,
    /// Directory treated as the config's anchor — parent of the loaded
    /// `.devcli.toml` when present, else the main worktree so linked
    /// worktrees inherit shared config/hooks as a single unit.
    pub config_root: Option<std::path::PathBuf>,
    /// Effective stem (after autodetect).
    pub stem: String,
    /// Effective base branch (after autodetect).
    pub base_branch: String,
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn unknown_keys_are_rejected() {
        // A valid config still parses.
        assert!(toml::from_str::<Config>("[workspace]\nmax_count = 5\n").is_ok());
        // A typo'd top-level section/key is a hard error (not silently ignored).
        assert!(toml::from_str::<Config>("notakey = 1\n").is_err());
        // A typo'd key inside a known section is also rejected.
        assert!(toml::from_str::<Config>("[workspace]\nmax_cont = 5\n").is_err());
    }
}
