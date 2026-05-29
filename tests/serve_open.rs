//! Red test: `cw serve start --open` must wait for the frontend port to
//! accept connections before invoking `open` on the URL.
//!
//! Condor's legacy `serve.sh open` already satisfies this contract by
//! passing `--open` to vite (with `BROWSER=open`), so vite spawns `open`
//! itself only after the dev server is ready. The Rust port currently
//! fires `open` immediately after spawning the service, which races the
//! server and shows a "connection refused" page on cold start.

use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

fn bin_in(dirs: &[&str], bin: &str) -> bool {
    dirs.iter().any(|d| Path::new(d).join(bin).is_file())
}

fn pick_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

fn make_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} failed in {}", dir.display());
}

fn kill_port(port: u16) {
    let _ = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "pids=$(lsof -ti :{port} 2>/dev/null); if [ -n \"$pids\" ]; then kill -9 $pids 2>/dev/null; fi"
        ))
        .status();
}

#[test]
fn serve_open_waits_for_frontend_port() {
    // Needs python3 (the mock dev server) + nc (the mock `open` readiness probe)
    // in the sandboxed PATH; skip rather than fail where they're absent.
    if !bin_in(&["/usr/bin", "/bin"], "python3") || !bin_in(&["/usr/bin", "/bin"], "nc") {
        eprintln!(
            "skipping serve_open_waits_for_frontend_port: needs python3 + nc in /usr/bin:/bin"
        );
        return;
    }
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let stem = "openwait";
    let number: u16 = 7;
    let port = pick_port();
    let base = port
        .checked_sub(number)
        .expect("pick_port returned < number");

    let repo = root.join(stem);
    fs::create_dir(&repo).unwrap();
    Command::new("git")
        .args(["init", "--initial-branch=develop", repo.to_str().unwrap()])
        .status()
        .unwrap();
    git(&repo, &["config", "user.email", "t@t.local"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);

    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let open_log = root.join("open.log");
    make_executable(
        &bin.join("open"),
        &format!(
            r#"#!/usr/bin/env bash
url="$1"
p="${{url##*:}}"
p="${{p%%/*}}"
if nc -z 127.0.0.1 "$p" 2>/dev/null; then
    printf 'listening %s\n' "$url" >"{log}"
else
    printf 'not_listening %s\n' "$url" >"{log}"
fi
"#,
            log = open_log.display()
        ),
    );

    let config = format!(
        r#"[workspace]
max_count = 48
stem = "{stem}"

[[services]]
name = "frontend"
subdir = "."
port = {{ base = {base} }}
start = "python3 -c 'import socket,time; time.sleep(1); s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind((\"127.0.0.1\", {port})); s.listen(64); time.sleep(30)'"
open_url = "http://localhost:{port}/"
"#
    );
    fs::write(repo.join(".devcli.toml"), &config).unwrap();
    fs::write(repo.join("README.md"), "r\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "init", "--quiet"]);

    let ws = root.join(format!("{stem}_{number}"));
    git(
        &repo,
        &[
            "worktree",
            "add",
            ws.to_str().unwrap(),
            "-b",
            "br",
            "develop",
        ],
    );

    let path_env = format!("{}:/usr/bin:/bin", bin.display());
    let mut cmd = Command::cargo_bin("cw").unwrap();
    cmd.args(["serve", "start", "--open"])
        .current_dir(&ws)
        .env("PATH", &path_env)
        .env_remove("CW_WRAPPER");
    let out = cmd.output().unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    while !open_log.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }

    let ok = out.status.success();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let log_content = fs::read_to_string(&open_log).unwrap_or_default();

    kill_port(port);

    assert!(ok, "cw serve failed\nstdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        open_log.exists(),
        "mock `open` was never invoked\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        log_content.starts_with("listening "),
        "browser opened before frontend port was ready — Condor's serve.sh waits via vite `--open`, Rust must wait too.\nopen.log: {log_content:?}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
