//! Daemonize a command chain: redirects stdio to a log, writes a
//! SETUP_DONE sentinel on exit, detaches via setsid so the child survives
//! the parent.

use anyhow::{Context, Result};
use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// Run the given shell command in the background, appending all output
/// (stdout+stderr) to `log`. Writes a final `SETUP_DONE rc=<n>` line on
/// completion so observers can detect termination.
///
/// The child is detached via `setsid` so it survives the cw process exit.
pub fn spawn_shell_detached(
    shell_cmd: &str,
    cwd: &Path,
    log: &Path,
    sentinel_tag: &str,
) -> Result<u32> {
    if let Some(p) = log.parent() {
        if !p.as_os_str().is_empty() {
            std::fs::create_dir_all(p).ok();
        }
    }
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .with_context(|| format!("opening {}", log.display()))?;
    let log_stderr = log_file.try_clone()?;

    // Wrap so the sentinel always prints — even on command failure.
    let wrapped = format!("{{ {shell_cmd}; }} ; printf '%s rc=%s\\n' '{sentinel_tag}' \"$?\"");

    let mut cmd = Command::new("bash");
    cmd.arg("-lc")
        .arg(&wrapped)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_stderr));
    unsafe {
        cmd.pre_exec(|| {
            let _ = nix::unistd::setsid();
            Ok(())
        });
    }
    let child = cmd
        .spawn()
        .with_context(|| format!("spawning detached shell in {}", cwd.display()))?;
    let pid = child.id();
    std::mem::forget(child);
    Ok(pid)
}
