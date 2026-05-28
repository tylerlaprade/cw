//! Log display: static tail (`logs -n N`) + follow mode (`tail -f`).

use super::processes::Ctx;
use crate::config::{schema::ServiceCfg, Config};
use crate::workspace::resolve::Resolved;
use anyhow::Result;
use std::io::{Read, Seek, SeekFrom};

pub fn show(
    cfg: &Config,
    resolved: &Resolved,
    services: &[&ServiceCfg],
    lines: Option<usize>,
    follow: bool,
) -> Result<()> {
    let n = lines.unwrap_or(50);
    let mut contexts = Vec::new();
    for svc in services {
        contexts.push(Ctx::build(cfg, resolved, svc)?);
    }

    // Print the last N lines of each log.
    for ctx in &contexts {
        println!("=== {} ({}) ===", ctx.display_name(), ctx.log_file.display());
        print_tail(&ctx.log_file, n)?;
    }

    if follow {
        follow_all(&contexts)?;
    }
    Ok(())
}

fn print_tail(path: &std::path::Path, n: usize) -> Result<()> {
    if !path.is_file() {
        println!("(no log yet)");
        return Ok(());
    }
    let text = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    for line in &lines[start..] {
        println!("{line}");
    }
    Ok(())
}

/// Follow every log, prefixing each line with the service name. Uses a
/// polling loop; good enough for a dev-server log tail.
fn follow_all(contexts: &[Ctx]) -> Result<()> {
    let mut files: Vec<_> = contexts
        .iter()
        .map(|c| {
            let f = std::fs::File::open(&c.log_file).ok();
            (c.svc.name.clone(), f, 0u64)
        })
        .collect();

    // Start at end-of-file.
    for (_, f, off) in files.iter_mut() {
        if let Some(f) = f {
            *off = f.seek(SeekFrom::End(0)).unwrap_or(0);
        }
    }

    let mut buf = [0u8; 4096];
    let mut leftover: Vec<(String, Vec<u8>)> =
        files.iter().map(|(n, _, _)| (n.clone(), Vec::new())).collect();
    loop {
        for ((name, f_opt, off), (_, pending)) in files.iter_mut().zip(leftover.iter_mut()) {
            // Re-open if file wasn't present at start.
            if f_opt.is_none() {
                if let Some(c) = contexts.iter().find(|c| c.svc.name == *name) {
                    *f_opt = std::fs::File::open(&c.log_file).ok();
                    if let Some(f) = f_opt {
                        *off = f.seek(SeekFrom::End(0)).unwrap_or(0);
                    }
                }
            }
            let Some(f) = f_opt else { continue };
            // Seek to last known offset (handles log rotation poorly, which is fine for now).
            let _ = f.seek(SeekFrom::Start(*off));
            match f.read(&mut buf) {
                Ok(0) => {}
                Ok(n) => {
                    *off += n as u64;
                    pending.extend_from_slice(&buf[..n]);
                    // Emit complete lines.
                    while let Some(pos) = pending.iter().position(|b| *b == b'\n') {
                        let line = pending.drain(..=pos).collect::<Vec<_>>();
                        let line = String::from_utf8_lossy(&line[..line.len() - 1]);
                        println!("[{}] {}", name, line);
                    }
                }
                Err(_) => {}
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

