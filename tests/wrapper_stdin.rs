//! Wrapper must not drain the user's stdin before EXEC'ing a TUI.
//!
//! Regression guard: the zsh/bash wrappers used to read records with
//! `while … <<< "$_out"`, which pinned the parse loop's stdin to the
//! record payload. Any EXEC inside then inherited that drained fd — so
//! a later patch added `EXEC "${_argv[@]}" </dev/tty` to paper over it.
//! That reopen broke Claude Code's TUI (Bun/Ink) which snapshots fd 0
//! at startup and expects it to be the controlling terminal directly,
//! not a fresh `/dev/tty` reference.
//!
//! Fix: iterate via array split (no here-string-on-stdin), drop the
//! `</dev/tty` workaround. This test feeds known bytes into the
//! wrapper on stdin, runs an EXEC that `cat`s its stdin, and asserts
//! the bytes round-trip through.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

fn template_path(shell: &str) -> std::path::PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    match shell {
        "zsh" => root.join("templates/zsh.sh"),
        "bash" => root.join("templates/bash.sh"),
        _ => panic!("unknown shell"),
    }
}

fn write_mock_cw(dir: &Path) {
    let path = dir.join("cw");
    fs::write(
        &path,
        "#!/bin/sh\n\
         # Mock cw binary: emit one EXEC record that cats stdin.\n\
         printf 'CW\\tEXEC\\tcat\\n'\n",
    )
    .unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
}

fn run_wrapper(shell: &str, mock_dir: &Path, stdin_bytes: &[u8]) -> (String, String, i32) {
    let template = template_path(shell);
    let script = format!(
        "export PATH=\"{}:$PATH\"\n\
         source \"{}\"\n\
         cw\n",
        mock_dir.display(),
        template.display()
    );
    let mut child = Command::new(shell)
        .arg("-c")
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {shell}: {e}"));
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin_bytes)
        .unwrap();
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn zsh_wrapper_passes_stdin_to_exec() {
    let tmp = tempfile::tempdir().unwrap();
    write_mock_cw(tmp.path());
    let (stdout, stderr, rc) = run_wrapper("zsh", tmp.path(), b"hello world\n");
    assert_eq!(rc, 0, "zsh wrapper exited {rc}, stderr={stderr}");
    assert!(
        stdout.contains("hello world"),
        "EXEC'd cat didn't see piped stdin. got stdout={stdout:?} stderr={stderr:?}"
    );
}

#[test]
fn bash_wrapper_passes_stdin_to_exec() {
    let tmp = tempfile::tempdir().unwrap();
    write_mock_cw(tmp.path());
    let (stdout, stderr, rc) = run_wrapper("bash", tmp.path(), b"hello world\n");
    assert_eq!(rc, 0, "bash wrapper exited {rc}, stderr={stderr}");
    assert!(
        stdout.contains("hello world"),
        "EXEC'd cat didn't see piped stdin. got stdout={stdout:?} stderr={stderr:?}"
    );
}
