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

    let cli = Cli::parse();
    let mut emitter = shell::Emitter::from_env();

    let rc = match cli.command {
        Command::ShellInit { shell } => shell::init::run(shell),
        Command::Doctor => doctor::run(),
        Command::Config { action } => config::command::run(action),
        Command::Serve(args) => serve::run(args, &mut emitter),
        Command::Open { target } => workspace::command::open(target, &mut emitter),
        Command::Restack(args) => restack::run(args, &mut emitter),
        Command::Remove(args) => workspace::command::remove(args, &mut emitter),
        Command::Cleanup(args) => cleanup::run(args, &mut emitter),
        Command::Triage(args) => triage::run(args),
        Command::Workspace(args) => workspace::command::dispatch(args, &mut emitter),
        Command::Init => config::init::run(),
        Command::Default(rest) => workspace::command::default_dispatch(rest, &mut emitter),
    };

    match rc {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("{} {:#}", "error:".red().bold(), e);
            std::process::exit(1);
        }
    }
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
