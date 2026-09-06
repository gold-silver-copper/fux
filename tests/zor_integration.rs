//! Required real-zor integration. Set `ZOR_BIN` to an independently built zor binary; with
//! `FUX_REQUIRE_ZOR_BIN=1` a missing binary fails instead of skipping.
#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

#[test]
fn independent_zor_observes_fux_panes_through_the_control_protocol() {
    let zor = std::env::var_os("ZOR_BIN");
    assert!(
        zor.is_some() || std::env::var_os("FUX_REQUIRE_ZOR_BIN").is_none(),
        "ZOR_BIN is required for this integration run"
    );
    let Some(zor) = zor else {
        eprintln!(
            "skipping: ZOR_BIN is not set (set FUX_REQUIRE_ZOR_BIN=1 to make this a failure)"
        );
        return;
    };
    let result = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/verify/observer.py"
        ))
        .arg(env!("CARGO_BIN_EXE_fux"))
        .arg(zor)
        .output()
        .expect("run isolated observer checks");
    assert!(
        result.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
