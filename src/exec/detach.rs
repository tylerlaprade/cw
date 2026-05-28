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
    cmd.arg("-c")
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
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

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
        let _g = env_lock().lock().unwrap();
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
        assert_eq!(
            captured.trim(),
            "UV=<unset>",
            "child still saw UV_WORKING_DIR"
        );
    }

    /// Red-green guard: the detached shell must not be a login shell. Login
    /// shells source `~/.bash_profile`, which imports direnv/asdf/nvm state
    /// the user configured for interactive sessions. Those imports leak
    /// variables that the setup chain then honors — which is how
    /// `UV_WORKING_DIR` keeps coming back from the dead. Proven red by
    /// flipping `-c` to `-lc` in `spawn_shell_detached`: the profile sets
    /// `FROM_PROFILE=leaked`, the assertion below sees it.
    #[test]
    fn does_not_source_bash_profile() {
        let _g = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let fake_home = tmp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        std::fs::write(
            fake_home.join(".bash_profile"),
            "export FROM_PROFILE=leaked\n",
        )
        .unwrap();
        std::fs::write(fake_home.join(".bashrc"), "export FROM_PROFILE=leaked\n").unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &fake_home);

        let log = tmp.path().join("setup.log");
        let out = tmp.path().join("env.out");
        let chain = format!(
            "printf 'P=%s\\n' \"${{FROM_PROFILE-<unset>}}\" > {}",
            out.display()
        );
        spawn_shell_detached(&chain, tmp.path(), &log, "SETUP_DONE", &[]).expect("spawn");
        wait_for_sentinel(&log);
        let captured = std::fs::read_to_string(&out).expect("env.out");

        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        assert_eq!(
            captured.trim(),
            "P=<unset>",
            "child sourced bash profile and imported FROM_PROFILE"
        );
    }
}
