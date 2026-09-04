#![cfg(unix)]
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "integration-test failures should retain their assertion context"
)]

use fux::control::Event;
use fux::host::{WorkspaceEventSink, WorkspaceHost};
use fux::state::{AgentState, PaneId};
use koh::server::{ChangeSignal, SessionHost as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
struct Sink(Arc<Mutex<Vec<Event>>>);

impl WorkspaceEventSink for Sink {
    fn publish(&self, event: Event) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event);
    }
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

#[test]
fn real_zor_report_updates_workspace_state_events_and_retains_the_completed_child_snapshot() {
    let Some(zor) = std::env::var_os("ZOR_BIN").map(PathBuf::from) else {
        assert!(
            std::env::var_os("FUX_REQUIRE_ZOR_BIN").is_none(),
            "ZOR_BIN is required for this integration run"
        );
        eprintln!("skipping real zor integration: ZOR_BIN is not set");
        return;
    };
    assert!(
        zor.is_file(),
        "ZOR_BIN does not name a file: {}",
        zor.display()
    );

    let root = TempDir(std::env::temp_dir().join(format!(
        "fux-zor-integration-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    )));
    let rules = root.0.join("rules");
    std::fs::create_dir_all(&rules).expect("create rule directory");
    std::fs::write(
        rules.join("test.toml"),
        "id='test'\nprompt_marker='>'\nblock_markers=[]\n[[rules]]\nid='ready'\nstate='working'\nregion='whole'\ncontains=['READY']\nvisible_working=true\n[[rules]]\nid='idle'\nstate='idle'\nregion='whole'\ncontains=['IDLE']\nvisible_idle=true\n",
    )
    .expect("write generated-report rule");

    // WorkspaceHost intentionally supplies only normal wrapper arguments. This executable shim
    // adds the captured rule fixture while still execing the exact binary selected by ZOR_BIN.
    let wrapper = root.0.join("zor-fixture");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexec {} --rules {} --agent test \"$@\"\n",
            shell_quote(&zor),
            shell_quote(&rules)
        ),
    )
    .expect("write zor fixture shim");
    let mut permissions = std::fs::metadata(&wrapper)
        .expect("fixture shim metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&wrapper, permissions).expect("make fixture shim executable");

    let (mut session, control) = WorkspaceHost::shared(
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "stty raw -echo; printf READY; dd bs=1 count=1 >/dev/null 2>&1; printf '\\rIDLE '; sleep 5; printf CHILD_DONE"
                .into(),
        ],
        32,
        Some(wrapper),
    )
    .expect("spawn workspace through zor");
    let events = Arc::new(Mutex::new(Vec::new()));
    control.set_event_sink(Arc::new(Sink(Arc::clone(&events))));
    session.attach_notify(ChangeSignal::default());

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut observed_working = false;
    let mut interrupted = false;
    loop {
        let state = session.snapshot();
        let pane = state
            .pane(PaneId(1))
            .expect("initial pane remains addressable");
        observed_working |= pane.agent.state == AgentState::Working;
        let locked_events = events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let working_position = locked_events.iter().position(|event| {
            matches!(
                event,
                Event::AgentState {
                    pane: 1,
                    new_state: fux::control::AgentStatus::Working,
                    ..
                }
            )
        });
        let idle_position = locked_events.iter().position(|event| {
            matches!(
                event,
                Event::AgentState {
                    pane: 1,
                    new_state: fux::control::AgentStatus::Idle,
                    ..
                }
            )
        });
        drop(locked_events);
        let saw_working_event = working_position.is_some();
        if observed_working && saw_working_event && !interrupted {
            // Input traverses fux -> zor -> the real child, which then exits and disconnects its
            // PTY while leaving the detachable workspace object available for recovery.
            session.input(b"x");
            interrupted = true;
        }
        let text = pane
            .cells
            .iter()
            .map(|cell| cell.text.as_str())
            .collect::<String>();
        // The real child completed after receiving input through both fux and zor, and the
        // detachable workspace retained its resulting screen. This is the closest local recovery
        // seam; it deliberately does not claim a real network outage/reconnect.
        if observed_working
            && working_position
                .zip(idle_position)
                .is_some_and(|(working, idle)| working < idle)
            && pane.agent.state == AgentState::Idle
            && pane.agent.flags.idle
            && text.contains("CHILD_DONE")
        {
            assert!(session.snapshot().pane(PaneId(1)).is_some_and(|pane| {
                pane.agent.state == AgentState::Idle
                    && pane.agent.flags.idle
                    && pane
                        .cells
                        .iter()
                        .map(|cell| cell.text.as_str())
                        .collect::<String>()
                        .contains("CHILD_DONE")
            }));
            break;
        }
        if Instant::now() >= deadline {
            session.shutdown();
            panic!(
                "real zor Working→Idle path did not converge within 10s: working_seen={observed_working}, working_event={working_position:?}, idle_event={idle_position:?}, input_sent={interrupted}, agent={:?}, exited={:?}",
                pane.agent.state, pane.agent.exited
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    session.shutdown();
}
