//! Isolated real-binary scenarios driven through Python harnesses. Each script owns a disposable
//! HOME/XDG root and its own processes; none touches personal sessions.
#![allow(clippy::panic)]

fn run(script: &str) {
    let output = std::process::Command::new("python3")
        .arg(format!(
            "{}/tests/verify/{script}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .arg(env!("CARGO_BIN_EXE_fux"))
        .output()
        .unwrap_or_else(|error| panic!("starting harness {script}: {error}"));
    assert!(
        output.status.success(),
        "{script} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn isolated_local_attachment_has_no_keys_and_preserves_sessions() {
    run("local_attachment.py");
}

#[test]
fn isolated_tty_cold_start_and_detach_need_no_credentials() {
    run("local_tty.py");
}

#[test]
fn incompatible_server_is_rejected_before_terminal_setup() {
    run("protocol_rejection.py");
}

#[test]
fn detach_sends_preceding_input_waits_for_exit_and_drops_the_suffix() {
    run("detach_drain.py");
}

#[test]
fn real_viewer_scenarios_cover_the_interaction_contract() {
    run("viewer.py");
}
