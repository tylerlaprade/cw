use std::path::PathBuf;
use std::process::Command;

#[test]
#[ignore = "Exact Condor suite passthrough; enable manually while closing parity gaps"]
fn exact_condor_cw_suite_runs_against_rust_adapter() {
    run_exact_suite("rust");
}

#[test]
#[ignore = "Exact Condor suite passthrough; enable manually while closing parity gaps"]
fn exact_condor_cw_suite_runs_against_legacy_from_cw_repo() {
    run_exact_suite("legacy");
}

fn run_exact_suite(impl_name: &str) {
    let Some(legacy_root) = std::env::var_os("CW_PARITY_LEGACY_ROOT") else {
        eprintln!("set CW_PARITY_LEGACY_ROOT to the Condor repo root to run this test");
        return;
    };

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = root.join("scripts/test-condor-legacy.sh");
    let bin = assert_cmd::cargo::cargo_bin("cw");

    let out = Command::new("bash")
        .current_dir(&root)
        .arg(script)
        .arg(impl_name)
        .arg("cw")
        .env("CW_PARITY_LEGACY_ROOT", legacy_root)
        .env("CW_PARITY_RUST_BIN", &bin)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
