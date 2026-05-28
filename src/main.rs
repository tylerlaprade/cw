mod cli;
mod config;
mod exec;
mod shell;
mod util;

mod cleanup;
mod git;
mod restack;
mod serve;
mod triage;
mod workspace;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Command};
use owo_colors::OwoColorize;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("CW_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .without_time()
        .with_target(false)
        .init();

    let mut emitter = shell::Emitter::from_env();

    // Route BEFORE clap: bare `cw <description|N|PR#|branch>` and every
    // leading-flag form (`cw -s …`, `cw --stack …`, `cw --pr N`, `cw --continue`)
    // are not clap subcommands — clap rejects a leading flag with "unexpected
    // argument", which used to make the documented stacking workflow unusable.
    let rc = match dispatcher_args(&std::env::args().collect::<Vec<_>>()) {
        Some(rest) => workspace::command::default_dispatch(rest, &mut emitter),
        None => run_subcommand(Cli::parse().command, &mut emitter),
    };

    match rc {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("{} {:#}", "error:".red().bold(), e);
            std::process::exit(1);
        }
    }
}

/// Known clap subcommand names. Anything else as the first arg (a number, a
/// description, or a leading flag) belongs to the bare dispatcher.
const SUBCOMMANDS: &[&str] = &[
    "shell-init",
    "doctor",
    "config",
    "serve",
    "open",
    "restack",
    "resolve",
    "remove",
    "cleanup",
    "triage",
    "workspace",
    "init",
    "completions",
];

/// Returns `Some(dispatcher_args)` when argv should go to `default_dispatch`,
/// or `None` when it's a real clap subcommand / help / version request.
fn dispatcher_args(argv: &[String]) -> Option<Vec<String>> {
    match argv.get(1).map(String::as_str) {
        None => Some(Vec::new()), // bare `cw` → dispatcher prints its own usage
        Some("-h" | "--help" | "-V" | "--version") => None,
        Some(first) if SUBCOMMANDS.contains(&first) => None,
        Some(_) => Some(argv[1..].to_vec()),
    }
}

#[cfg(test)]
mod route_tests {
    use super::dispatcher_args;

    fn argv(parts: &[&str]) -> Vec<String> {
        std::iter::once("cw")
            .chain(parts.iter().copied())
            .map(String::from)
            .collect()
    }

    #[test]
    fn leading_flags_route_to_dispatcher() {
        // The bug: clap rejected leading flags. They must reach the dispatcher.
        for case in [&["-s", "fix bug"][..], &["--stack", "x"], &["--pr", "7"], &["--continue"]] {
            assert!(
                dispatcher_args(&argv(case)).is_some(),
                "{case:?} should route to the dispatcher"
            );
        }
    }

    #[test]
    fn descriptions_and_numbers_route_to_dispatcher() {
        assert!(dispatcher_args(&argv(&["fix", "the", "thing"])).is_some());
        assert!(dispatcher_args(&argv(&["5"])).is_some());
        assert!(dispatcher_args(&argv(&["7543"])).is_some());
    }

    #[test]
    fn bare_cw_routes_to_dispatcher_help() {
        assert_eq!(dispatcher_args(&argv(&[])), Some(Vec::new()));
    }

    #[test]
    fn subcommands_and_help_go_to_clap() {
        for case in [
            &["serve", "start"][..],
            &["restack"],
            &["workspace", "list"],
            &["remove", "3"],
            &["triage"],
            &["-h"],
            &["--help"],
            &["--version"],
        ] {
            assert!(
                dispatcher_args(&argv(case)).is_none(),
                "{case:?} should go to clap"
            );
        }
    }
}

fn run_subcommand(command: Command, emitter: &mut shell::Emitter) -> Result<()> {
    match command {
        Command::ShellInit { shell } => shell::init::run(shell),
        Command::Doctor => doctor::run(),
        Command::Config { action } => config::command::run(action),
        Command::Serve(args) => serve::run(args, emitter),
        Command::Open { target } => workspace::command::open(target, emitter),
        Command::Restack(args) => restack::run(args, emitter),
        Command::Resolve(args) => restack::resolve_cmd(args),
        Command::Remove(args) => workspace::command::remove(args, emitter),
        Command::Cleanup(args) => cleanup::run(args, emitter),
        Command::Triage(args) => triage::run(args),
        Command::Workspace(args) => workspace::command::dispatch(args, emitter),
        Command::Init => config::init::run(),
        Command::Completions { shell } => emit_completions(shell),
    }
}

fn emit_completions(shell: cli::Shell) -> anyhow::Result<()> {
    use clap::CommandFactory;
    use clap_complete::{generate, shells};
    let mut cmd = cli::Cli::command();
    match shell {
        cli::Shell::Zsh => generate(shells::Zsh, &mut cmd, "cw", &mut std::io::stdout()),
        cli::Shell::Bash => generate(shells::Bash, &mut cmd, "cw", &mut std::io::stdout()),
        cli::Shell::Fish => generate(shells::Fish, &mut cmd, "cw", &mut std::io::stdout()),
    }
    Ok(())
}

mod doctor {
    use anyhow::Result;
    use owo_colors::OwoColorize;

    const PROBES: &[(&str, &str)] = &[
        ("git", "required"),
        ("gt", "Graphite integration (optional)"),
        ("gh", "GitHub PR resolution + triage (optional)"),
        ("acli", "Jira triage (optional)"),
        ("claude", "restack resolver + workspace launch (optional)"),
        ("codex", "restack resolver (optional)"),
        ("psql", "database clone/drop (optional)"),
        ("direnv", "workspace .envrc allow (optional)"),
    ];

    pub fn run() -> Result<()> {
        for (bin, note) in PROBES {
            match which(bin) {
                Some(p) => println!(
                    "{} {} {} {}",
                    "✓".green(),
                    bin,
                    p.display().to_string().dimmed(),
                    format!("— {}", note).dimmed()
                ),
                None => println!("{} {} {}", "✗".red(), bin, format!("— {}", note).dimmed()),
            }
        }
        Ok(())
    }

    fn which(bin: &str) -> Option<std::path::PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path).find_map(|d| {
            let candidate = d.join(bin);
            if candidate.is_file() {
                Some(candidate)
            } else {
                None
            }
        })
    }
}
