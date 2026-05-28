use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Clone, Copy)]
pub enum Runner {
    Rust,
    Legacy,
}

pub fn legacy_root() -> Option<PathBuf> {
    std::env::var_os("CW_PARITY_LEGACY_ROOT").map(PathBuf::from)
}

pub fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "git {:?} failed in {}",
        args,
        dir.display()
    );
}

pub fn init_repo(dir: &Path) {
    git(
        dir.parent().unwrap(),
        &["init", "--initial-branch=develop", dir.to_str().unwrap()],
    );
    git(dir, &["config", "user.email", "test@test.local"]);
    git(dir, &["config", "user.name", "Test"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

pub fn commit_file(dir: &Path, rel: &str, contents: &str, msg: &str) {
    fs::write(dir.join(rel), contents).unwrap();
    git(dir, &["add", rel]);
    git(dir, &["commit", "-m", msg, "--quiet"]);
}

pub fn make_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

fn copy_executable(src: &Path, dst: &Path) {
    fs::copy(src, dst).unwrap();
    let mut perms = fs::metadata(dst).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(dst, perms).unwrap();
}

pub fn install_legacy_scripts(worktree: &Path) {
    let legacy_root = legacy_root().expect("set CW_PARITY_LEGACY_ROOT to run legacy tests");
    let scripts = worktree.join("scripts");
    fs::create_dir_all(&scripts).unwrap();
    copy_executable(&legacy_root.join("scripts/cw.sh"), &scripts.join("cw.sh"));
    copy_executable(
        &legacy_root.join("scripts/worktree-lib.sh"),
        &scripts.join("worktree-lib.sh"),
    );
    make_executable(
        &scripts.join("new-workspace.sh"),
        "#!/usr/bin/env bash\nexit 0\n",
    );
    make_executable(&worktree.join("serve.sh"), "#!/usr/bin/env bash\nexit 0\n");
    make_executable(
        &worktree.join("restack.sh"),
        "#!/usr/bin/env bash\nexit 0\n",
    );
}

pub fn add_worktree(repo: &Path, ws: &Path, branch: &str, runner: Runner) {
    git(
        repo,
        &[
            "worktree",
            "add",
            ws.to_str().unwrap(),
            "-b",
            branch,
            "develop",
        ],
    );
    if matches!(runner, Runner::Legacy) {
        install_legacy_scripts(ws);
    }
}

pub fn run_cw(
    runner: Runner,
    repo: &Path,
    path: &str,
    extra_env: &[(&str, &str)],
    args: &[&str],
) -> Output {
    let mut cmd = match runner {
        Runner::Rust => Command::cargo_bin("cw").unwrap(),
        Runner::Legacy => {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let script = root.join("scripts/compat/rust/run-legacy-cw.sh");
            let mut cmd = Command::new("bash");
            cmd.arg(script);
            cmd
        }
    };

    cmd.current_dir(repo).env("PATH", path);
    if matches!(runner, Runner::Legacy) {
        let legacy_root = legacy_root().expect("set CW_PARITY_LEGACY_ROOT to run legacy tests");
        cmd.env("CW_PARITY_LEGACY_ROOT", legacy_root);
    }
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    cmd.args(args).output().unwrap()
}

pub fn combined_output(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}
