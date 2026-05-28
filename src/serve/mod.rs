//! Dev-server lifecycle manager.
//!
//! `cw serve <action> [target]`. Action ∈ start | stop | restart | status |
//! logs | tail. Target resolves via `workspace::resolve`.

pub mod logs;
pub mod processes;

use crate::cli::ServeArgs;
use crate::config::{self, schema::ServiceCfg};
use crate::shell::{Emitter, Record};
use crate::workspace::resolve::{resolve, Resolved};
use anyhow::{Context, Result};
use owo_colors::OwoColorize;

pub fn run(args: ServeArgs, emitter: &mut Emitter) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let cfg = config::discover::load(&cwd)?;
    let resolved = resolve(&cfg, &cwd, args.target.as_deref())?;
    let services = select_services(&cfg, args.service.as_deref())?;

    match args.action.as_str() {
        "start" => start(&cfg, &resolved, &services, &args, emitter),
        "stop" => stop(&cfg, &resolved, &services, emitter),
        "restart" => {
            let _ = stop(&cfg, &resolved, &services, emitter);
            start(&cfg, &resolved, &services, &args, emitter)
        }
        "status" => status(&cfg, &resolved, &services),
        "logs" => logs::show(&cfg, &resolved, &services, args.lines, false),
        "tail" => logs::show(&cfg, &resolved, &services, args.lines, true),
        other => anyhow::bail!(
            "unknown serve action: {other} (expected start|stop|restart|status|logs|tail)"
        ),
    }
}

fn select_services<'a>(
    cfg: &'a crate::config::Config,
    filter: Option<&str>,
) -> Result<Vec<&'a ServiceCfg>> {
    if cfg.services.is_empty() {
        anyhow::bail!(
            "no services detected in {}: add a [[services]] entry to .devcli.toml or run `cw config show` to inspect autodetection",
            cfg.runtime.repo_root.as_deref().map(|p| p.display().to_string()).unwrap_or_default()
        );
    }
    match filter {
        None => Ok(cfg.services.iter().collect()),
        Some(name) => {
            let matched: Vec<_> = cfg
                .services
                .iter()
                .filter(|s| s.name == name || s.alias.iter().any(|a| a == name))
                .collect();
            if matched.is_empty() {
                anyhow::bail!(
                    "no service matching {name:?}; known: {}",
                    cfg.services
                        .iter()
                        .map(|s| s.name.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            Ok(matched)
        }
    }
}

fn start(
    cfg: &crate::config::Config,
    resolved: &Resolved,
    services: &[&ServiceCfg],
    args: &ServeArgs,
    emitter: &mut Emitter,
) -> Result<()> {
    for svc in services {
        let ctx = processes::Ctx::build(cfg, resolved, svc)?;
        match processes::start(&ctx, args.no_ai) {
            Ok(pid) => {
                let msg = format!(
                    "{} {} started pid={} port={} log={}",
                    "✓".green(),
                    ctx.display_name(),
                    pid,
                    ctx.port,
                    ctx.log_file.display()
                );
                if emitter.enabled() {
                    emitter.emit(Record::Msg(&msg));
                } else {
                    println!("{msg}");
                }
            }
            Err(e) => {
                eprintln!("{} {}: {:#}", "✗".red(), ctx.display_name(), e);
            }
        }
    }
    // G4: open the browser BEFORE entering the (blocking) follow loop —
    // otherwise `--tail --open` never opens anything.
    if args.open {
        for svc in services {
            if let Some(url) = &svc.open_url {
                let ctx = processes::Ctx::build(cfg, resolved, svc)?;
                // Don't race the dev server on cold start: poll TCP connect for
                // readiness before launching the browser.
                wait_port_listening(ctx.port, std::time::Duration::from_secs(60));
                open_in_browser(&ctx.expand(url));
            }
        }
    }
    if args.tail {
        logs::show(cfg, resolved, services, args.lines, true)?;
    }
    Ok(())
}

/// Open a URL in the default browser, cross-platform, surfacing failures.
fn open_in_browser(url: &str) {
    use std::process::Command;
    let (prog, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("open", &[])
    } else if cfg!(target_os = "windows") {
        // `cmd /c start "" <url>` — the empty title avoids start treating the
        // URL as a window title.
        ("cmd", &["/c", "start", ""])
    } else {
        ("xdg-open", &[])
    };
    let status = Command::new(prog).args(args).arg(url).status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => eprintln!(
            "{} couldn't open browser ({prog} exited {}). URL: {url}",
            "⚠".yellow(),
            s.code().unwrap_or(-1)
        ),
        Err(e) => eprintln!(
            "{} couldn't open browser ({prog}: {e}). URL: {url}",
            "⚠".yellow()
        ),
    }
}

fn wait_port_listening(port: u16, timeout: std::time::Duration) -> bool {
    use std::net::TcpStream;
    use std::time::Instant;
    let addr = match format!("127.0.0.1:{port}").parse() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(200)).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

fn stop(
    cfg: &crate::config::Config,
    resolved: &Resolved,
    services: &[&ServiceCfg],
    emitter: &mut Emitter,
) -> Result<()> {
    for svc in services {
        let ctx = processes::Ctx::build(cfg, resolved, svc)?;
        let outcome = processes::stop(&ctx);
        let msg = format!("{} {} {}", "✓".green(), ctx.display_name(), outcome);
        if emitter.enabled() {
            emitter.emit(Record::Msg(&msg));
        } else {
            println!("{msg}");
        }
    }
    Ok(())
}

fn status(
    cfg: &crate::config::Config,
    resolved: &Resolved,
    services: &[&ServiceCfg],
) -> Result<()> {
    for svc in services {
        let ctx = processes::Ctx::build(cfg, resolved, svc)?;
        let st = processes::status(&ctx);
        let label = match st {
            processes::Status::Running(pid) => format!("{} pid={pid}", "running".green()),
            processes::Status::Stopped => format!("{}", "stopped".dimmed()),
            processes::Status::StalePid(pid) => format!("{} pid={pid}", "stale".yellow()),
        };
        println!(
            "{:12} port={} pid_file={} {}",
            ctx.display_name(),
            ctx.port,
            ctx.pid_file.display(),
            label
        );
    }
    Ok(())
}

pub(crate) fn pid_from_file(path: &std::path::Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

pub(crate) fn expand_template(
    template: &str,
    stem: &str,
    number: u32,
    port: u16,
    extra: &[(&str, &str)],
) -> String {
    let mut s = template
        .replace("{stem}", stem)
        .replace("{n}", &number.to_string())
        .replace("{port}", &port.to_string());
    for (k, v) in extra {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

pub(crate) fn ensure_parent(path: &std::path::Path) -> Result<()> {
    if let Some(p) = path.parent() {
        if !p.as_os_str().is_empty() {
            std::fs::create_dir_all(p).with_context(|| format!("creating {}", p.display()))?;
        }
    }
    Ok(())
}
