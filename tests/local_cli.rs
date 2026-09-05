#![allow(clippy::panic)]

fn run(script: &str) {
    let output = std::process::Command::new("python3")
        .arg(format!(
            "{}/tests/verify/{script}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .arg(env!("CARGO_BIN_EXE_fux"))
        .output()
        .unwrap_or_else(|error| panic!("starting local CLI harness: {error}"));
    assert!(
        output.status.success(),
        "{}\n{}",
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
fn contextual_help_repaints_a_silent_pane_without_slowing_fast_commands() {
    run("contextual_help.py");
}

#[test]
fn contextual_modes_remain_private_and_usable_across_terminal_resizes() {
    run("contextual_viewers.py");
}
