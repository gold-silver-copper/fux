#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use fux::control::{
    ErrorCode, MAX_FRAME_BYTES, Request, WorkspaceAction, decode_request_frame, read_request,
};
use fux::state::{MAX_CLIPBOARD_BYTES, PaneId, PaneView, WorkspaceState};
use std::io::Cursor;

#[test]
fn hostile_control_frames_are_bounded_and_the_stream_recovers() {
    // Phase I security: oversized and unknown input is rejected without desynchronizing the next frame.
    let mut bytes = vec![b'x'; MAX_FRAME_BYTES + 1];
    bytes.extend_from_slice(b"\n{\"command\":\"list\",\"id\":7}\n");
    let mut input = Cursor::new(bytes);
    assert_eq!(
        read_request(&mut input).expect_err("oversized").code,
        ErrorCode::FrameTooLarge
    );
    assert_eq!(
        read_request(&mut input)
            .expect("read")
            .expect("request")
            .id(),
        7
    );
    for frame in [
        b"{}".as_slice(),
        b"{\"command\":\"future\",\"id\":9}",
        b"{\"command\":\"list\",\"id\":9,\"extra\":[]}",
    ] {
        assert!(decode_request_frame(frame).is_err());
    }
}

#[test]
fn workspace_names_cannot_escape_runtime_directories() {
    // Phase I security: traversal and separator-bearing workspace names never validate.
    for name in [
        "",
        ".",
        "..",
        "../escape",
        "/absolute",
        "a/b",
        "a\\b",
        "nul\0name",
    ] {
        let request = Request::Workspace {
            id: 1,
            action: WorkspaceAction::New { name: name.into() },
        };
        assert!(request.validate().is_err(), "accepted {name:?}");
    }
}

#[test]
fn peer_state_bounds_reject_oversized_cells_and_clipboards() {
    // Phase I security: decoded peer state cannot request unchecked cell or clipboard allocation.
    let mut state = WorkspaceState::default();
    let oversized = PaneView {
        rows: u16::MAX,
        columns: u16::MAX,
        cells: Vec::new(),
        ..PaneView::default()
    };
    assert!(state.insert_pane(PaneId(1), oversized).is_err());
    let result = state.update_metadata(|metadata| {
        metadata.clipboard_base64 = "x".repeat(MAX_CLIPBOARD_BYTES + 1)
    });
    assert!(result.is_err());
}

#[test]
fn pane_osc_reports_are_spoofable_but_strictly_parsed() {
    // Phase I threat model: the pane process is the OSC trust boundary, not an authenticated reporter.
    let valid = b"7877;v=1;state=blocked;agent=claude;seq=4;visible=blocker";
    let report = fux::parse_agent_report(valid).expect("valid self-report");
    assert_eq!(report.state(), zor::osc::State::Blocked);
    for invalid in [
        b"7877;v=1;state=blocked;seq=4".as_slice(),
        b"7877;v=1;state=admin;agent=x;seq=1",
        b"7877;v=1;state=idle;agent=x;seq=1;seq=2",
    ] {
        assert!(fux::parse_agent_report(invalid).is_err());
    }
}
