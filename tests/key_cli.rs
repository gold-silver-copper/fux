#![allow(clippy::panic)]

#[test]
fn isolated_identity_cli_and_pty_regressions() {
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/verify/key_prompts.py"
        ))
        .arg(env!("CARGO_BIN_EXE_fux"))
        .output()
        .unwrap_or_else(|error| panic!("starting Python PTY regression harness: {error}"));
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
