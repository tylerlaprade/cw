use std::process::Command;

fn run_parity(mode: &str, extra_env: &[(&str, &str)]) {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = root.join("scripts/test-parity.sh");

    let mut cmd = Command::new("bash");
    cmd.current_dir(&root).arg(script).arg(mode);
    for (key, value) in extra_env {
        cmd.env(key, value);
    }

    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn parity_contract_holds_for_rust_driver() {
    let bin = assert_cmd::cargo::cargo_bin("cw");
    run_parity("rust", &[("CW_PARITY_RUST_BIN", bin.to_str().unwrap())]);
}

#[test]
fn parity_contract_holds_for_legacy_driver_when_available() {
    let Some(root) = std::env::var_os("CW_PARITY_LEGACY_ROOT") else {
        eprintln!("skipping legacy parity: set CW_PARITY_LEGACY_ROOT to enable");
        return;
    };
    run_parity(
        "legacy",
        &[("CW_PARITY_LEGACY_ROOT", root.to_str().unwrap())],
    );
}
