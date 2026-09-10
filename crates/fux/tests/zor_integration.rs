//! Required real-zor integration. Explicit binary ownership is mandatory in the combined gate.
#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]
mod support;

fn run(script: &str) {
    // Preserve the original sequential execution and its resource assumptions.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    if matches!(script, "test_zor_contention" | "test_owned_processes") {
        let root = support::workspace_root();
        let filter = if script == "test_zor_contention" {
            "support::contention::tests"
        } else {
            "support::process::tests"
        };
        let result = std::process::Command::new("cargo")
            .args(["test", "--locked", "--manifest-path"])
            .arg(root.join("tools/xtask/Cargo.toml"))
            .arg("--target-dir")
            .arg(root.join("target/rust-harness"))
            .args(["--lib", filter])
            .output()
            .expect("run Rust support regressions");
        assert!(
            result.status.success(),
            "{script}:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let mut command = std::process::Command::new(support::rust_harness());
    command.args(["scenario", &script.replace('_', "-")]);
    command.arg(env!("CARGO_BIN_EXE_fux")).arg(&zor);
    if script == "zor_headless" {
        command.arg(support::zor_argv_fixture());
    }
    if matches!(script, "zor_native" | "zor_workflow") {
        command.arg(support::zor_codex_fixture());
    }
    if script == "zor_dashboard" {
        command.arg(support::zor_notifier_fixture());
    }
    let result = command.output().expect("run isolated integration checks");
    assert!(
        result.status.success(),
        "{script}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn independent_zor_observes_fux_panes_through_the_control_protocol() {
    run("observer");
}

// Separate entry points preserve every required scenario and allow targeted migration checks.
macro_rules! scenarios {
    ($($name:ident),* $(,)?) => { $(#[test] fn $name() { run(stringify!($name)); })* };
}
scenarios!(
    test_zor_contention,
    test_owned_processes,
    zor_service,
    zor_events,
    zor_tasks,
    zor_bindings,
    zor_producers,
    zor_launch,
    zor_headless,
    zor_native,
    zor_worktree,
    zor_checks,
    zor_check_workers,
    zor_changes,
    zor_sources,
    zor_workflow,
    zor_groups,
    zor_group_scheduler,
    zor_recovery,
    zor_dashboard
);
