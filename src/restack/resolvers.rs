//! Built-in conflict resolvers: claude, codex, manual.

use anyhow::Result;
use owo_colors::OwoColorize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy)]
pub enum Kind {
    Claude,
    Codex,
    Manual,
}

impl Kind {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "claude" => Self::Claude,
            "codex" => Self::Codex,
            "manual" => Self::Manual,
            _ => Self::Manual,
        }
    }

    pub fn autodetect() -> Self {
        if crate::util::in_path("claude") {
            Self::Claude
        } else if crate::util::in_path("codex") {
            Self::Codex
        } else {
            Self::Manual
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Manual => "manual",
        }
    }
}

pub fn run(kind: Kind, dir: &Path, files: &[PathBuf]) -> Result<()> {
    println!(
        "{} resolver {} on {} file(s)",
        "→".cyan(),
        kind.name(),
        files.len()
    );
    match kind {
        Kind::Claude => run_claude(dir, files),
        Kind::Codex => run_codex(dir, files),
        Kind::Manual => run_manual(dir, files),
    }
}

fn file_list(files: &[PathBuf]) -> String {
    files
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn run_claude(dir: &Path, files: &[PathBuf]) -> Result<()> {
    let prompt = format!(
        "Resolve the merge conflicts in these files, preserving the intent of both sides: {}",
        file_list(files)
    );
    let st = Command::new("claude")
        .args(["-p", &prompt, "--permission-mode", "acceptEdits"])
        .current_dir(dir)
        .status()?;
    if !st.success() {
        eprintln!("{} claude exited {}", "⚠".yellow(), st.code().unwrap_or(-1));
    }
    Ok(())
}

fn run_codex(dir: &Path, files: &[PathBuf]) -> Result<()> {
    let prompt = format!(
        "Resolve the merge conflicts in these files, preserving the intent of both sides: {}",
        file_list(files)
    );
    let st = Command::new("codex")
        .args(["exec", &prompt])
        .current_dir(dir)
        .status()?;
    if !st.success() {
        eprintln!("{} codex exited {}", "⚠".yellow(), st.code().unwrap_or(-1));
    }
    Ok(())
}

fn run_manual(_dir: &Path, files: &[PathBuf]) -> Result<()> {
    println!("Unresolved files:");
    for f in files {
        println!("  {}", f.display());
    }
    println!(
        "\nResolve the conflicts by hand, then re-run {} to continue the rebase. \
         State lives in git; `cw restack` is idempotent.",
        "cw restack".bold()
    );
    Ok(())
}
