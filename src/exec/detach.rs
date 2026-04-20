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
///
/// `env_strip` names environment variables to remove before exec. The
/// setup path uses this to drop caller-inherited `UV_WORKING_DIR` — mirroring
/// `new-workspace.sh` — so `uv run --script` can't chdir into the source
/// workspace and make relative paths in the chain resolve to the wrong tree.
pub fn spawn_shell_detached(
    shell_cmd: &str,
    cwd: &Path,
    log: &Path,
    sentinel_tag: &str,
    env_strip: &[&str],
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
    for var in env_strip {
        cmd.env_remove(var);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_for_sentinel(log: &Path) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(s) = std::fs::read_to_string(log) {
                if s.contains("SETUP_DONE rc=") {
                    return s;
                }
            }
            if Instant::now() >= deadline {
                panic!("sentinel never appeared in {}", log.display());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Red-green guard: caller-inherited UV_WORKING_DIR must be stripped
    /// before exec so `uv run --script` (or anything else that honors it)
    /// doesn't chdir out of the setup chain's workspace. Proven red by
    /// removing the `env_remove` loop in `spawn_shell_detached` — the
    /// captured file gains a line `UV=/nope` and the assertion fires.
    #[test]
    fn strips_env_vars_before_exec() {
        std::env::set_var("UV_WORKING_DIR", "/nope");
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("setup.log");
        let out = tmp.path().join("env.out");
        let chain = format!(
            "printf 'UV=%s\\n' \"${{UV_WORKING_DIR-<unset>}}\" > {}",
            out.display()
        );
        spawn_shell_detached(&chain, tmp.path(), &log, "SETUP_DONE", &["UV_WORKING_DIR"])
            .expect("spawn");
        wait_for_sentinel(&log);
        let captured = std::fs::read_to_string(&out).expect("env.out");
        std::env::remove_var("UV_WORKING_DIR");
        assert_eq!(captured.trim(), "UV=<unset>", "child still saw UV_WORKING_DIR");
    }
}
