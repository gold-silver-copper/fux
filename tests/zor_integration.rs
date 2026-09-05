#![cfg(unix)]
#![allow(
    clippy::expect_used,
    reason = "integration failures retain subprocess diagnostics"
)]

#[test]
fn optional_observers_never_own_the_pane_process() {
    let zor = std::env::var_os("ZOR_BIN");
    assert!(
        zor.is_some() || std::env::var_os("FUX_REQUIRE_ZOR_BIN").is_none(),
        "ZOR_BIN is required for this integration run"
    );
    let mut command = std::process::Command::new("python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/verify/observer.py"
        ))
        .arg(env!("CARGO_BIN_EXE_fux"));
    if let Some(zor) = zor {
        command.arg(zor);
    }
    let result = command.output().expect("run isolated observer checks");
    assert!(
        result.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
