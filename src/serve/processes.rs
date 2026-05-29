//! Process lifecycle: start (daemonized via process_group(0)), stop
//! (layered SIGTERM → SIGKILL → pkill → lsof), status.

use super::{ensure_parent, expand_template, pid_from_file};
use crate::config::{schema::ServiceCfg, Config};
use crate::workspace::resolve::Resolved;
use anyhow::{Context, Result};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub struct Ctx<'a> {
    pub stem: String,
    pub number: u32,
    pub port: u16,
    pub cwd: PathBuf,
    pub svc: &'a ServiceCfg,
    pub pid_file: PathBuf,
    pub log_file: PathBuf,
    pub start_cmd: String,
}

pub enum Status {
    Running(u32),
    Stopped,
    StalePid(u32),
}

impl<'a> Ctx<'a> {
    pub fn build(cfg: &Config, resolved: &Resolved, svc: &'a ServiceCfg) -> Result<Self> {
        let stem = cfg.runtime.stem.clone();
        let number = resolved.number.unwrap_or(0);
        let base = svc
            .port
            .as_ref()
            .context("service has no [port.base]")?
            .base;
        // Compute in u32 so a large workspace number can't silently truncate
        // (number as u16 before the add would wrap); error if it exceeds 65535.
        let raw_port = u32::from(base)
            .checked_add(number)
            .context("port computation overflowed")?;
        let port = u16::try_from(raw_port).with_context(|| {
            format!("port {raw_port} exceeds 65535 (workspace number {number} too large)")
        })?;
        let subdir = svc.subdir.as_deref().unwrap_or(".");
        let cwd = resolved.dir.join(subdir);
        let pid_file = PathBuf::from(expand_template(
            svc.pid_file
                .as_deref()
                .unwrap_or("/tmp/{stem}_{n}_{svc}.pid"),
            &stem,
            number,
            port,
            &[("svc", &svc.name)],
        ));
        let log_file = PathBuf::from(expand_template(
            svc.log_file
                .as_deref()
                .unwrap_or("/tmp/{stem}_{n}_{svc}.log"),
            &stem,
            number,
            port,
            &[("svc", &svc.name)],
        ));
        let start_cmd = expand_template(
            svc.start
                .as_deref()
                .context("service has no start command")?,
            &stem,
            number,
            port,
            &[("svc", &svc.name)],
        );
        Ok(Self {
            stem,
            number,
            port,
            cwd,
            svc,
            pid_file,
            log_file,
            start_cmd,
        })
    }

    pub fn display_name(&self) -> String {
        format!("[{}]", self.svc.name)
    }

    pub fn expand(&self, s: &str) -> String {
        expand_template(s, &self.stem, self.number, self.port, &[])
    }
}

pub fn status(ctx: &Ctx) -> Status {
    let Some(pid) = pid_from_file(&ctx.pid_file) else {
        return Status::Stopped;
    };
    if pid_alive(pid) {
        Status::Running(pid)
    } else {
        Status::StalePid(pid)
    }
}

pub fn start(ctx: &Ctx, no_ai: bool) -> Result<u32> {
    match status(ctx) {
        Status::Running(pid) => anyhow::bail!("already running (pid {})", pid),
        // A dead PID's file is stale — clear it so the claim below can succeed.
        Status::StalePid(_) => {
            let _ = std::fs::remove_file(&ctx.pid_file);
        }
        Status::Stopped => {}
    }

    ensure_parent(&ctx.log_file)?;
    ensure_parent(&ctx.pid_file)?;

    if !ctx.cwd.is_dir() {
        anyhow::bail!("service cwd not found: {}", ctx.cwd.display());
    }

    // `{ai_mode}` ("true"/"false") is exposed to BOTH the pre_start hook and the
    // start command, so a service can branch on AI/QA mode — e.g. append
    // `--noreload` (Django) or `--no-watch` for a stable snapshot under `--no-ai`.
    let ai_mode = if no_ai { "false" } else { "true" };

    // Fire the pre_start shell snippet if present (best-effort).
    if let Some(snippet) = &ctx.svc.pre_start {
        let snippet = expand_template(
            snippet,
            &ctx.stem,
            ctx.number,
            ctx.port,
            &[("ai_mode", ai_mode)],
        );
        let st = Command::new("bash")
            .arg("-c")
            .arg(&snippet)
            .current_dir(&ctx.cwd)
            .status();
        if let Err(e) = st {
            eprintln!("pre_start failed: {e:#}");
        }
    }

    // Wrap the start command in bash so `source .venv/bin/activate && exec ...`
    // works when a venv is configured. `exec` ensures the child we record the
    // PID of is the real server, not the wrapping bash.
    let mut shell_cmd = String::new();
    if let Some(venv) = &ctx.svc.venv {
        let venv_path = ctx.cwd.join(venv);
        if venv_path.is_file() {
            shell_cmd.push_str(&format!(
                "source {} && ",
                shell_quote(&venv_path.display().to_string())
            ));
        }
    }
    shell_cmd.push_str("exec ");
    // `{ai_mode}` isn't substituted at Ctx::build time, so fill it in now.
    shell_cmd.push_str(&ctx.start_cmd.replace("{ai_mode}", ai_mode));

    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&ctx.log_file)
        .with_context(|| format!("opening {}", ctx.log_file.display()))?;
    let log_stderr = log.try_clone()?;

    // Atomically claim the PID file right before spawning: concurrent starts
    // can both pass status() above, but only one wins create_new — closing the
    // double-start race. The real PID overwrites this placeholder after spawn.
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&ctx.pid_file)
    {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            anyhow::bail!("another start is already in progress");
        }
        Err(e) => return Err(e).with_context(|| format!("claiming {}", ctx.pid_file.display())),
    }

    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(&shell_cmd)
        .current_dir(&ctx.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_stderr));
    for (k, v) in &ctx.svc.start_env {
        cmd.env(k, v);
    }
    // Detach: new process group so SIGHUP to parent doesn't kill the child.
    unsafe {
        cmd.pre_exec(|| {
            // setsid detaches from the controlling terminal and starts a new session.
            let _ = nix::unistd::setsid();
            Ok(())
        });
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            // Release the claim so a retry isn't blocked by our placeholder.
            let _ = std::fs::remove_file(&ctx.pid_file);
            return Err(e).with_context(|| format!("spawning service in {}", ctx.cwd.display()));
        }
    };
    let pid = child.id();
    std::fs::write(&ctx.pid_file, format!("{pid}\n"))
        .with_context(|| format!("writing {}", ctx.pid_file.display()))?;
    // We intentionally do NOT wait on the child — it owns its own session now.
    std::mem::forget(child);
    Ok(pid)
}

pub fn stop(ctx: &Ctx) -> &'static str {
    let mut killed_any = false;

    if let Some(pid) = pid_from_file(&ctx.pid_file) {
        // Only signal the recorded PID if it's alive AND still our service
        // (owns the port). After our server dies the PID can be reused by an
        // unrelated process; the port-bound fallbacks below cover the rest.
        if pid_alive(pid) && pid_owns_port(pid, ctx.port) {
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
            if wait_for_exit(pid, 20, 250) {
                killed_any = true;
            } else {
                let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
                if wait_for_exit(pid, 20, 100) {
                    killed_any = true;
                }
            }
        }
        let _ = std::fs::remove_file(&ctx.pid_file);
    }

    // Fallback 1: pkill -f <pattern> for each configured stop pattern.
    for pattern in &ctx.svc.stop_patterns {
        let expanded = expand_template(pattern, &ctx.stem, ctx.number, ctx.port, &[]);
        let out = Command::new("pkill").args(["-f", &expanded]).status();
        if matches!(out, Ok(s) if s.success()) {
            killed_any = true;
        }
    }

    // Fallback 2: lsof -ti:PORT | xargs kill -9.
    let lsof = Command::new("lsof")
        .args(["-ti", &format!(":{}", ctx.port)])
        .output();
    if let Ok(out) = lsof {
        if out.status.success() {
            for pid_s in String::from_utf8_lossy(&out.stdout).split_whitespace() {
                if let Ok(pid) = pid_s.parse::<i32>() {
                    let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
                    killed_any = true;
                }
            }
        }
    }

    if killed_any {
        "stopped"
    } else {
        "already stopped"
    }
}

fn pid_alive(pid: u32) -> bool {
    // signal 0 = existence check, no signal delivered
    kill(Pid::from_raw(pid as i32), None).is_ok()
}

/// True if `pid` is among the processes listening on `port` (`lsof -ti:port`).
/// Used to confirm a recorded PID is still our service before killing it.
fn pid_owns_port(pid: u32, port: u16) -> bool {
    let out = Command::new("lsof")
        .args(["-ti", &format!(":{port}")])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .filter_map(|s| s.parse::<u32>().ok())
            .any(|p| p == pid),
        _ => false,
    }
}

fn wait_for_exit(pid: u32, tries: u32, pause_ms: u64) -> bool {
    for _ in 0..tries {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(pause_ms));
    }
    !pid_alive(pid)
}

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+@=,:".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}
