//! Subprocess seam. Concrete implementations live in submodules; all
//! subprocess-heavy code takes `&impl ShellExecutor` so tests can mock.

pub mod detach;

use anyhow::Result;

pub trait ShellExecutor {
    fn run(&self, argv: &[&str]) -> Result<std::process::Output>;
    fn run_in(&self, cwd: &std::path::Path, argv: &[&str]) -> Result<std::process::Output>;
}

pub struct RealExec;

impl ShellExecutor for RealExec {
    fn run(&self, argv: &[&str]) -> Result<std::process::Output> {
        let (head, tail) = argv.split_first().ok_or_else(|| anyhow::anyhow!("empty argv"))?;
        Ok(std::process::Command::new(head).args(tail).output()?)
    }

    fn run_in(&self, cwd: &std::path::Path, argv: &[&str]) -> Result<std::process::Output> {
        let (head, tail) = argv.split_first().ok_or_else(|| anyhow::anyhow!("empty argv"))?;
        Ok(std::process::Command::new(head)
            .args(tail)
            .current_dir(cwd)
            .output()?)
    }
}
