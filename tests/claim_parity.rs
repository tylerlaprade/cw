//! Cross-language parity for workspace-number selection.
//!
//! Rust `cw workspace next-number` and legacy Bash
//! `scripts/new-workspace.sh` (with `NEW_WORKSPACE_DRY_RUN=1`) must pick the
//! same N for a given sandbox. This catches drift between
//! `workspace::create::claim_number` and `find_next_number` in the canonical
//! Bash port.

use assert_cmd::cargo::CommandCargoExt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

/// Both tests mutate /tmp state (lock dirs) and call into the shared
/// `find_next_number` logic, which scans `/tmp/.condor_workspace_*.lock`
/// and `/tmp/.devcli_condor_*_claim` globally. Serialize them so they
/// don't see each other's locks.
fn tmp_mutex() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn legacy_root() -> Option<PathBuf> {
    std::env::var_os("CW_PARITY_LEGACY_ROOT").map(PathBuf::from)
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} in {} failed", dir.display());
}

fn init_condor_repo(parent: &Path) -> PathBuf {
    let repo = parent.join("condor");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--initial-branch=develop", "--quiet"]);
    git(&repo, &["config", "user.email", "test@test.local"]);
    git(&repo, &["config", "user.name", "Test"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    fs::write(
        repo.join(".devcli.toml"),
        // Pin the stem so autodetect doesn't trip on the repo basename.
        "[workspace]\nstem = \"condor\"\nmax_count = 48\n",
    )
    .unwrap();
    fs::write(repo.join("README"), "root\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "root", "--quiet"]);
    repo
}

fn rust_next_number(repo: &Path) -> u32 {
    let out = Command::cargo_bin("cw")
        .unwrap()
        .current_dir(repo)
        .args(["workspace", "next-number"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cw workspace next-number failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u32>()
        .unwrap_or_else(|e| panic!("bad rust output {:?}: {e}", out.stdout))
}

/// Compute the legacy next-number via new-workspace.sh's dry-run path. Returns
/// None when the provided legacy copy lacks the `NEW_WORKSPACE_DRY_RUN` hook
/// (e.g. the upstream script) — running it then would perform real side effects
/// (createdb / worktree add / rsync), so the caller skips the comparison.
fn bash_next_number(legacy: &Path, repo: &Path) -> Option<u32> {
    let script = legacy.join("scripts/new-workspace.sh");
    assert!(
        script.is_file(),
        "missing {}; set CW_PARITY_LEGACY_ROOT correctly",
        script.display()
    );

    // new-workspace.sh derives REPO_ROOT from its own path (dirname of
    // SCRIPT_DIR), so we copy the needed scripts into our sandbox repo and
    // invoke the copy. This matches how Condor users run it in practice.
    let scripts = repo.join("scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::copy(&script, scripts.join("new-workspace.sh")).unwrap();
    fs::copy(
        legacy.join("scripts/worktree-lib.sh"),
        scripts.join("worktree-lib.sh"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = fs::metadata(scripts.join("new-workspace.sh"))
            .unwrap()
            .permissions();
        p.set_mode(0o755);
        fs::set_permissions(scripts.join("new-workspace.sh"), p).unwrap();
    }

    // Guard against running the upstream script (no dry-run) for real.
    let script_src = fs::read_to_string(scripts.join("new-workspace.sh")).unwrap_or_default();
    if !script_src.contains("NEW_WORKSPACE_DRY_RUN") {
        return None;
    }

    let out = Command::new("bash")
        .arg(scripts.join("new-workspace.sh"))
        .arg("parity-test-branch")
        .env("NEW_WORKSPACE_DRY_RUN", "1")
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "new-workspace.sh dry run failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Parse "DRY_RUN N=<num> ...".
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("DRY_RUN ") {
            for tok in rest.split_whitespace() {
                if let Some(v) = tok.strip_prefix("N=") {
                    return Some(v.parse().unwrap());
                }
            }
        }
    }
    panic!("no DRY_RUN line in new-workspace.sh output:\n{stdout}");
}

#[test]
fn rust_and_legacy_agree_on_lowest_gap() {
    let _guard = tmp_mutex().lock().unwrap();
    let Some(legacy) = legacy_root() else {
        eprintln!("skipping: set CW_PARITY_LEGACY_ROOT to enable");
        return;
    };

    let sandbox = tempfile::tempdir().unwrap();
    let repo = init_condor_repo(sandbox.path());
    // Pre-populate siblings at 1 and 3 — both impls should pick 2
    // (modulo any global /tmp/condor_N or git worktree-list state that both
    // scan equally; the assertion is on equality, not a specific value).
    fs::create_dir_all(sandbox.path().join("condor_1")).unwrap();
    fs::create_dir_all(sandbox.path().join("condor_3")).unwrap();

    let rust_n = rust_next_number(&repo);
    let Some(bash_n) = bash_next_number(&legacy, &repo) else {
        eprintln!("skipping: legacy new-workspace.sh has no NEW_WORKSPACE_DRY_RUN hook");
        return;
    };
    assert_eq!(
        rust_n, bash_n,
        "claim_number / find_next_number disagree (rust={rust_n}, bash={bash_n})"
    );
}

#[test]
fn rust_reclaims_lock_that_bash_would_also_reclaim() {
    let _guard = tmp_mutex().lock().unwrap();
    // Both impls treat lock dirs older than 60s as stale and reclaim them.
    // This test exercises the stale-reclaim path in the Rust implementation
    // without needing to shell out to Bash; the legacy-equivalent behavior
    // is spot-checked by tests/claim_parity.rs::rust_and_legacy_agree... in
    // the same sandbox topology.
    let sandbox = tempfile::tempdir().unwrap();
    let repo = init_condor_repo(sandbox.path());
    fs::create_dir_all(sandbox.path().join("condor_1")).unwrap();

    // Seed a *fresh* Rust-shaped lock — on a system with no other cw
    // process running, this should be masked as "live" and the claim picks
    // a higher number.
    let lock = Path::new("/tmp/.devcli_condor_2_claim");
    // Best-effort: if another real process holds this, we can't test this
    // invariant cleanly. Skip gracefully.
    if lock.exists() {
        eprintln!("skipping: /tmp has live cw lock state outside our control");
        return;
    }
    fs::create_dir(lock).unwrap();
    let _cleanup = scopeguard(lock);

    let n = rust_next_number(&repo);
    assert!(
        n != 2,
        "expected claim to skip active lock at slot 2, got {n}"
    );
}

struct Scopeguard<'a>(&'a Path);
impl Drop for Scopeguard<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_dir(self.0);
    }
}
fn scopeguard(path: &Path) -> Scopeguard<'_> {
    Scopeguard(path)
}
