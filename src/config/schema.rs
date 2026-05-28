use serde::{Deserialize, Serialize};

/// The effective (possibly-autodetected) config. Everything is optional at the
/// file level; defaults are filled in by the autodetect pass.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
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

    /// Computed at runtime, not read from file.
    #[serde(skip)]
    pub runtime: Runtime,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct WorkspaceCfg {
    /// Maximum workspace number (default: unlimited).
    pub max_count: Option<u32>,
    /// Override auto-detected base branch (default: develop|main|master).
    pub base_branch: Option<String>,
    /// Override auto-detected stem (default: repo-root basename).
    pub stem: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Integrations {
    pub graphite: Option<bool>,
    pub github: Option<bool>,
    pub claude: Option<bool>,
    pub codex: Option<bool>,
    pub direnv: Option<bool>,
    pub acli: Option<bool>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
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
pub struct PortCfg {
    pub base: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepsCfg {
    #[serde(default = "yes")]
    pub parallel: bool,
    pub install: Vec<DepInstall>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepInstall {
    pub dir: String,
    pub cmd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabasesCfg {
    /// e.g. "app_{n}_{suffix}"
    pub pattern: String,
    pub suffixes: Vec<String>,
    /// "postgres" or "none"
    #[serde(default = "default_clone")]
    pub clone: String,
    #[serde(default = "default_src_suffix")]
    pub default_source_suffix: String,
}

fn default_clone() -> String {
    "postgres".into()
}
fn default_src_suffix() -> String {
    "qa".into()
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
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
pub struct HooksCfg {
    pub post_create: Option<String>,
    pub pre_remove: Option<String>,
    pub post_cd: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
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
pub struct EnvStrip {
    pub file: String,
    pub patterns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvInject {
    pub file: String,
    pub line: String,
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
