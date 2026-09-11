//! Isolated real-binary scenarios run through the standalone Rust harness. Each scenario owns a disposable
//! HOME/XDG root and its own processes; none touches personal sessions.
#![allow(clippy::panic)]
mod support;

fn run(scenario: &str) {
    let output = std::process::Command::new(support::rust_harness())
        .args(["scenario", scenario, env!("CARGO_BIN_EXE_fux")])
        .output()
        .unwrap_or_else(|error| panic!("starting harness {scenario}: {error}"));
    assert!(
        output.status.success(),
        "{scenario} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn isolated_local_attachment_has_no_keys_and_preserves_sessions() {
    run("local-attachment");
}

#[test]
fn isolated_tty_cold_start_and_detach_need_no_credentials() {
    run("local-tty");
}

#[test]
fn rejected_handshake_leaves_the_terminal_untouched() {
    run("protocol-rejection");
}

#[test]
fn detach_sends_preceding_input_waits_for_exit_and_drops_the_suffix() {
    run("detach-drain");
}

#[test]
fn real_viewer_scenarios_cover_the_interaction_contract() {
    run("viewer");
}

#[test]
fn a_full_agent_session_runs_headlessly_over_the_control_protocol() {
    run("control-workflow");
}

#[test]
fn input_receipts_regressions() {
    run("input-receipts");
}

#[test]
fn event_sync_regressions() {
    run("event-sync");
}

#[test]
fn final_records_regressions() {
    run("final-records");
}

#[test]
fn utf8_capture_regressions() {
    run("utf8-capture");
}

#[test]
fn empty_arguments_regressions() {
    run("empty-arguments");
}

#[test]
fn incompatible_manager_rejection_preserves_existing_sessions() {
    run("migration");
}
