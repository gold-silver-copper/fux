//! Isolated real-binary scenarios, migrating to Rust. Each scenario owns a disposable
//! HOME/XDG root and its own processes; none touches personal sessions.
#![allow(clippy::panic)]
mod support;

fn run(script: &str) {
    let output = std::process::Command::new(support::rust_harness())
        .args(["scenario", script, env!("CARGO_BIN_EXE_fux")])
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
    run("local-attachment");
}

#[test]
fn isolated_tty_cold_start_and_detach_need_no_credentials() {
    run("local-tty");
}

#[test]
fn incompatible_server_is_rejected_before_terminal_setup() {
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
fn incompatible_server_dialog_keeps_the_old_server_unless_confirmed() {
    run("migration");
}

#[test]
fn input_receipts_survive_reconnect_and_bound_a_stalled_pty() {
    run("input-receipts");
}

#[test]
fn event_snapshots_replay_without_gaps_or_duplicates() {
    run("event-sync");
}

#[test]
fn final_pane_evidence_remains_available_after_workspace_closes() {
    run("final-records");
}

#[test]
fn split_utf8_preserves_following_output_in_real_captures() {
    run("utf8-capture");
}

#[test]
fn empty_arguments_reach_configured_new_and_split_processes() {
    run("empty-arguments");
}
