#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use fux::client::backend::TerminalBackend;
use fux::client::view::{Overlay, OverlayCell};
use fux::client::{
    CaptureBackend, ClientNotificationGate, Compositor, CopyMode, DetachFilter, Selection,
    WorkspaceTerminal, client_notification_command,
};
use fux::client::{ClientState, ClientTerminal};
use fux::state::{
    AgentStatus, Cell, CellKind, CellStyle, LayoutTree, PaneId, PaneView, Popup, Tab, TabId,
    WorkspaceState,
};

#[test]
fn client_notifications_baseline_and_reconnect_do_not_duplicate_transitions() {
    use fux::state::AgentState::{Blocked, Idle, Working};
    let mut gate = ClientNotificationGate::new(fux::config::NotificationPolicy::default());
    assert!(
        !gate.observe(1, Idle),
        "initial state establishes a baseline"
    );
    assert!(!gate.observe(1, Working));
    assert!(gate.observe(1, Blocked));
    assert!(
        !gate.observe(1, Blocked),
        "replay/reconnect is not a transition"
    );
    assert!(gate.observe(1, Idle));
    assert!(!gate.observe(1, Idle));
    let mut first_blocked = ClientNotificationGate::new(fux::config::NotificationPolicy::default());
    assert!(first_blocked.observe(9, Blocked));
    first_blocked.retain([]);
    assert_eq!(first_blocked.tracked_count(), 0);
}

#[test]
fn macos_client_notifier_falls_back_to_osascript() {
    let command = client_notification_command("blocked \\\"now", false, true, false, |name| {
        name == "osascript"
    })
    .expect("osascript fallback");
    assert_eq!(command.first().map(String::as_str), Some("osascript"));
    assert!(command.get(2).is_some_and(|script| {
        script.contains("display notification") && script.contains("blocked \\\\\\\"now")
    }));
}

#[test]
fn workspace_client_state_exposes_window_modes_and_exit_status() {
    // Focused panes supply multiplexer terminal modes.
    let mut state = workspace();
    state
        .update_pane(PaneId(1), |pane| {
            pane.modes.application_cursor = true;
            pane.modes.application_keypad = true;
            pane.modes.bracketed_paste = true;
        })
        .expect("update pane");
    state
        .update_metadata(|metadata| {
            metadata.window_title = "work".into();
            metadata.clipboard_base64 = "aGk=".into();
            metadata.bell_count = 3;
            metadata.exit_code = Some(7);
        })
        .expect("metadata");
    let window = state.window();
    assert_eq!(
        (window.title, window.clipboard, window.bell_count),
        ("work", "aGk=", 3)
    );
    assert_eq!(state.exit_code(), Some(7));
    let modes = state.input_modes();
    assert!(modes.application_cursor && modes.application_keypad && modes.bracketed_paste);
    assert_eq!(modes.mouse_encoding, vt100::MouseProtocolEncoding::Sgr);
}

#[test]
fn compositor_handles_tiny_frames_wide_cells_prediction_popups_status_and_selection() {
    // Phase F3 compositor: clipping and layer order remain safe at every terminal size.
    let mut state = workspace();
    state
        .update_pane(PaneId(1), |pane| {
            pane.cells[0] = Cell {
                text: "界".into(),
                kind: CellKind::WideLeading,
                style: CellStyle::default(),
            };
            pane.cells[1] = Cell {
                text: String::new(),
                kind: CellKind::WideContinuation,
                style: CellStyle::default(),
            };
            pane.cursor.column = 2;
        })
        .expect("wide pane");
    let overlay = Overlay {
        cells: std::collections::BTreeMap::from([(
            (0, 1),
            OverlayCell {
                glyph: "y".into(),
                fg: vt100::Color::Default,
                bg: vt100::Color::Default,
                underline: false,
                unknown: false,
            },
        )]),
        cursor: Some((0, 2)),
    };
    assert_eq!(
        overlay.cell(0, 1).map(|cell| cell.glyph.as_str()),
        Some("y")
    );
    let mut compositor = Compositor::default();
    let frame = compositor.compose(&state, &overlay, Some("connected"), 8, 20);
    assert_eq!(
        frame.buffer.cell((2, 1)).map(|cell| cell.symbol()),
        Some("y")
    );
    assert!(row_text(&frame.buffer, 7).contains("connected"));
    compositor.set_selection(Some(Selection {
        start: (1, 1),
        end: (1, 3),
    }));
    let selected = compositor.compose(&state, &Overlay::empty(), None, 8, 20);
    assert!(!compositor.selected_text(&selected).is_empty());
    for (rows, cols) in [(0, 0), (1, 1), (2, 1), (1, 8), (80, 240)] {
        let frame = compositor.compose(&state, &Overlay::empty(), None, rows, cols);
        assert_eq!(frame.buffer.area.height, rows);
        assert_eq!(frame.buffer.area.width, cols);
    }

    let popup_pane = text_pane(2, 8, "popup");
    state
        .insert_pane(PaneId(2), popup_pane)
        .expect("popup pane");
    state
        .replace_popups(vec![Popup {
            pane: PaneId(2),
            width: 10,
            height: 4,
            z_index: 2,
        }])
        .expect("popup");
    let popup = compositor.compose(&state, &Overlay::empty(), None, 8, 20);
    assert!(
        popup
            .buffer
            .content()
            .iter()
            .any(|cell| cell.symbol() == "p")
    );
}

#[test]
fn compositor_surfaces_agent_state_and_emphasizes_blocked_panes_at_tiny_widths() {
    let mut state = workspace();
    state
        .update_pane(PaneId(1), |pane| {
            pane.agent.id = Some("claude".into());
            pane.agent.state = fux::state::AgentState::Blocked;
        })
        .expect("agent");
    let frame = Compositor::default().compose(&state, &Overlay::empty(), None, 6, 40);
    assert!(
        frame
            .buffer
            .content()
            .iter()
            .any(|cell| cell.fg == ratatui_core::style::Color::Red)
    );
    let status: String = (0..40)
        .filter_map(|column| frame.buffer.cell((column, 5)))
        .map(|cell| cell.symbol())
        .collect();
    assert!(status.contains("claude:Blocked"));
    for width in 0..=4 {
        let _ = Compositor::default().compose(&state, &Overlay::empty(), None, 1, width);
    }
}

#[test]
fn terminal_mirrors_oob_deltas_frames_and_restores_modes_around_suspend() {
    // Phase F3 terminal: title/clipboard/bell/modes are coalesced and restoration is exact.
    let mut state = workspace();
    state
        .update_metadata(|metadata| {
            metadata.window_title = "bad\u{1b}]title".into();
            metadata.clipboard_base64 = "aGk=".into();
            metadata.bell_count = 4;
        })
        .expect("metadata");
    let backend = CaptureBackend::new(8, 20);
    let mut terminal = WorkspaceTerminal::enter(backend, true).expect("enter");
    terminal.backend_mut().bytes.clear();
    terminal
        .render(&state, &Overlay::empty(), None)
        .expect("render");
    let first = terminal.backend().bytes.clone();
    assert!(first.windows(8).any(|bytes| bytes == b"]0;bad]t"));
    assert!(first.windows(10).any(|bytes| bytes == b"]52;c;aGk="));
    assert_eq!(first.iter().filter(|byte| **byte == 7).count(), 3); // title + clipboard terminators + one bell
    terminal.backend_mut().bytes.clear();
    terminal
        .render(&state, &Overlay::empty(), None)
        .expect("second render");
    let second = &terminal.backend().bytes;
    assert!(!second.windows(4).any(|bytes| bytes == b"]52;"));
    assert!(!second.contains(&7));
    terminal.leave_for_suspend().expect("leave");
    assert!(!terminal.backend().raw && !terminal.backend().alternate);
    terminal.reenter_after_resume().expect("resume");
    assert!(terminal.backend().raw && terminal.backend().alternate);
}

#[derive(Debug)]
struct FaultBackend {
    inner: CaptureBackend,
    writes: usize,
    fail_at: usize,
}

impl TerminalBackend for FaultBackend {
    fn write_bytes(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.writes += 1;
        if self.writes == self.fail_at {
            return Err(std::io::Error::other("injected frame write failure"));
        }
        self.inner.write_bytes(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
    fn enter_raw_mode(&mut self) -> std::io::Result<()> {
        self.inner.enter_raw_mode()
    }
    fn leave_raw_mode(&mut self) -> std::io::Result<()> {
        self.inner.leave_raw_mode()
    }
    fn size(&self) -> std::io::Result<(u16, u16)> {
        self.inner.size()
    }
    fn write_input_modes(&mut self, _bytes: &[u8]) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn terminal_closes_synchronized_frame_after_mid_paint_error() {
    let backend = FaultBackend {
        inner: CaptureBackend::new(8, 20),
        writes: 0,
        // enter-alt is write 1; begin-frame is write 2; fail the first paint move.
        fail_at: 3,
    };
    let mut terminal = WorkspaceTerminal::enter(backend, false).expect("enter");
    assert!(
        terminal
            .render(&workspace(), &Overlay::empty(), None)
            .is_err()
    );
    assert!(
        terminal
            .backend()
            .inner
            .bytes
            .windows(8)
            .any(|w| w == b"\x1b[?2026l")
    );
}

struct EnterFaultBackend {
    bytes: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    raw: std::sync::Arc<std::sync::atomic::AtomicBool>,
    failed: bool,
}

impl TerminalBackend for EnterFaultBackend {
    fn write_bytes(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if !self.failed && bytes == b"\x1b[?1049h\x1b[?25l" {
            self.failed = true;
            self.bytes
                .lock()
                .expect("bytes")
                .extend_from_slice(b"\x1b[?1049h");
            return Err(std::io::Error::other("partial enter failure"));
        }
        self.bytes.lock().expect("bytes").extend_from_slice(bytes);
        Ok(())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    fn enter_raw_mode(&mut self) -> std::io::Result<()> {
        self.raw.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn leave_raw_mode(&mut self) -> std::io::Result<()> {
        self.raw.store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    fn size(&self) -> std::io::Result<(u16, u16)> {
        Ok((8, 20))
    }
}

#[test]
fn terminal_entry_failure_restores_screen_cursor_and_raw_mode() {
    let bytes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let raw = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let result = WorkspaceTerminal::enter(
        EnterFaultBackend {
            bytes: bytes.clone(),
            raw: raw.clone(),
            failed: false,
        },
        false,
    );
    assert!(result.is_err());
    assert!(!raw.load(std::sync::atomic::Ordering::SeqCst));
    let bytes = bytes.lock().expect("bytes");
    assert!(bytes.windows(8).any(|window| window == b"\x1b[?1049l"));
    assert!(bytes.windows(6).any(|window| window == b"\x1b[?25h"));
}

#[test]
fn detach_mapping_is_stable_at_every_chunk_boundary_and_paste_is_verbatim() {
    // Phase F3 detach: prefix-d maps client-side to koh's escape across arbitrary chunks.
    let input = b"aa\x01dbb\x01xcc";
    let expected = b"aabb\x01xcc";
    for split in 0..=input.len() {
        let mut filter = DetachFilter::new(vec![1]).expect("filter");
        let mut output = filter.process(&input[..split], false);
        output.extend(filter.process(&input[split..], false));
        output.extend(filter.flush());
        assert_eq!(output, expected, "split {split}");
        assert!(filter.take_detach());
    }
    let mut filter = DetachFilter::new(vec![1]).expect("filter");
    assert_eq!(filter.process(b"\x01d", true), b"\x01d");

    let bracketed = b"before\x1b[200~paste\x01d\x1b[201~after\x01d";
    let expected = b"before\x1b[200~paste\x01d\x1b[201~after";
    for split in 0..=bracketed.len() {
        let mut filter = DetachFilter::new(vec![1]).expect("filter");
        let mut output = filter.process_terminal_input(&bracketed[..split]);
        output.extend(filter.process_terminal_input(&bracketed[split..]));
        output.extend(filter.flush());
        assert_eq!(output, expected, "paste split {split}");
        assert!(filter.take_detach());
    }
}

#[test]
fn workspace_picker_mapping_is_opt_in_and_suppresses_the_server_side_tab_fallback() {
    let mut ordinary = DetachFilter::new(vec![1]).expect("filter");
    assert_eq!(ordinary.process(b"\x01s", false), b"\x01s");
    assert!(!ordinary.take_workspace_picker());

    let mut picker = DetachFilter::new(vec![1]).expect("filter");
    picker.set_workspace_picker_enabled(true);
    assert!(picker.process(b"\x01s", false).is_empty());
    assert!(picker.take_workspace_picker());
    assert!(!picker.take_workspace_picker());
}

#[test]
fn resize_repaints_stably_without_reusing_out_of_bounds_old_cells() {
    // Phase F3 compositor: terminal growth and shrink invalidate only the needed cell coordinates.
    let state = workspace();
    let backend = CaptureBackend::new(6, 12);
    let mut terminal = WorkspaceTerminal::enter(backend, false).expect("enter");
    terminal
        .render(&state, &Overlay::empty(), None)
        .expect("first");
    terminal.backend_mut().rows = 2;
    terminal.backend_mut().cols = 3;
    terminal
        .render(&state, &Overlay::empty(), None)
        .expect("shrink");
    terminal.backend_mut().rows = 10;
    terminal.backend_mut().cols = 40;
    terminal
        .render(&state, &Overlay::empty(), None)
        .expect("grow");
}

#[test]
fn copy_mode_extracts_wrapped_wide_and_combining_cells_and_sets_osc52_state() {
    let mut state = workspace();
    state
        .update_pane(PaneId(1), |pane| {
            pane.rows = 2;
            pane.columns = 4;
            pane.cells = vec![Cell::default(); 8];
            pane.wrapped_rows = vec![true, false];
            pane.cells[0] = Cell {
                text: "a\u{301}".into(),
                kind: CellKind::Text,
                style: CellStyle::default(),
            };
            pane.cells[1] = Cell {
                text: "界".into(),
                kind: CellKind::WideLeading,
                style: CellStyle::default(),
            };
            pane.cells[2] = Cell {
                text: String::new(),
                kind: CellKind::WideContinuation,
                style: CellStyle::default(),
            };
            pane.cells[3] = Cell {
                text: "b".into(),
                kind: CellKind::Text,
                style: CellStyle::default(),
            };
            pane.cells[4] = Cell {
                text: "c".into(),
                kind: CellKind::Text,
                style: CellStyle::default(),
            };
        })
        .expect("copy pane");
    let pane = state.pane(PaneId(1)).expect("pane").clone();
    let mut copy = CopyMode::default();
    copy.enter(&pane);
    assert!(copy.shift_drag(0, 0, false, &pane));
    assert!(copy.shift_drag(1, 0, true, &pane));
    assert_eq!(copy.selected_text(&pane), "a\u{301}界bc");
    assert!(copy.key(b"y", &mut state, PaneId(1)));
    assert_eq!(state.metadata().clipboard_base64, "YcyB55WMYmM=");
    let backend = CaptureBackend::new(6, 12);
    let mut terminal = WorkspaceTerminal::enter(backend, true).expect("terminal");
    terminal.backend_mut().bytes.clear();
    terminal
        .render(&state, &Overlay::empty(), None)
        .expect("render clipboard");
    let osc52 = b"]52;c;YcyB55WMYmM=";
    assert!(
        terminal
            .backend()
            .bytes
            .windows(osc52.len())
            .any(|bytes| bytes == osc52)
    );
}

#[test]
fn copy_keyboard_navigation_and_shared_scroll_survive_resize() {
    let mut state = workspace();
    let mut copy = CopyMode::default();
    copy.enter(state.pane(PaneId(1)).expect("pane"));
    assert!(copy.key(b"u", &mut state, PaneId(1)));
    assert_eq!(
        state.pane(PaneId(1)).map(|pane| pane.viewport_offset),
        Some(3)
    );
    assert!(copy.key(b" ", &mut state, PaneId(1)));
    assert!(copy.key(b"l", &mut state, PaneId(1)));
    assert!(copy.key(b"j", &mut state, PaneId(1)));
    let synchronized = state.pane(PaneId(1)).expect("synchronized pane").copy;
    assert!(synchronized.active);
    assert_eq!(synchronized.anchor, Some((0, 0)));
    assert_eq!(
        (synchronized.cursor_row, synchronized.cursor_column),
        (1, 1)
    );
    let composed = Compositor::default().compose(&state, &Overlay::empty(), None, 6, 12);
    assert!(
        composed.cursor.is_some(),
        "copy cursor replaces the application cursor"
    );
    assert!(
        composed.buffer.content().iter().any(|cell| {
            cell.modifier
                .contains(ratatui_core::style::Modifier::REVERSED)
        }),
        "synchronized selection is painted by the production compositor"
    );
    let backend = CaptureBackend::new(6, 12);
    let mut terminal = WorkspaceTerminal::enter(backend, false).expect("terminal");
    terminal
        .render(&state, &Overlay::empty(), None)
        .expect("scrolled render");
    terminal.backend_mut().rows = 3;
    terminal.backend_mut().cols = 5;
    terminal
        .render(&state, &Overlay::empty(), None)
        .expect("resize scrolled");
    assert_eq!(
        state.pane(PaneId(1)).map(|pane| pane.viewport_offset),
        Some(3)
    );
}

#[test]
fn copy_mode_incrementally_parses_split_and_batched_keys_and_consumes_unknowns() {
    let mut state = workspace();
    let mut copy = CopyMode::default();
    copy.enter(state.pane(PaneId(1)).expect("pane"));
    assert!(copy.key(b"", &mut state, PaneId(1)));
    assert!(copy.key(b"\x1b", &mut state, PaneId(1)));
    assert!(copy.key(b"[", &mut state, PaneId(1)));
    assert!(copy.key(b"Bjj?", &mut state, PaneId(1)));
    let synchronized = state.pane(PaneId(1)).expect("pane").copy;
    assert_eq!(synchronized.cursor_row, 3);
    assert!(synchronized.active);

    // A non-arrow byte following a pending escape exits without leaking either byte.
    assert!(copy.key(b"\x1b", &mut state, PaneId(1)));
    assert!(copy.key(b"x", &mut state, PaneId(1)));
    assert!(!state.pane(PaneId(1)).expect("pane").copy.active);
}

#[test]
fn terminal_paint_preserves_wide_glyph_continuations_on_initial_and_replacement_frames() {
    let mut state = workspace();
    state
        .update_pane(PaneId(1), |pane| {
            pane.cells[0] = Cell {
                text: "界".into(),
                kind: CellKind::WideLeading,
                style: CellStyle::default(),
            };
            pane.cells[1] = Cell {
                text: String::new(),
                kind: CellKind::WideContinuation,
                style: CellStyle::default(),
            };
        })
        .expect("wide pane");
    let mut terminal = WorkspaceTerminal::enter(CaptureBackend::new(6, 12), false).expect("enter");
    terminal.backend_mut().bytes.clear();
    terminal
        .render(&state, &Overlay::empty(), None)
        .expect("initial wide frame");
    assert_eq!(
        terminal
            .backend()
            .bytes
            .windows(3)
            .filter(|w| *w == "界".as_bytes())
            .count(),
        1
    );

    state
        .update_pane(PaneId(1), |pane| pane.cells[0].text = "語".into())
        .expect("replace wide pane");
    terminal.backend_mut().bytes.clear();
    terminal
        .render(&state, &Overlay::empty(), None)
        .expect("replacement wide frame");
    assert_eq!(
        terminal
            .backend()
            .bytes
            .windows(3)
            .filter(|w| *w == "語".as_bytes())
            .count(),
        1
    );
}

#[test]
fn status_bar_marks_only_the_active_tab() {
    let mut state = workspace();
    state
        .insert_pane(PaneId(2), text_pane(4, 12, "second"))
        .expect("second pane");
    state
        .replace_tabs(
            vec![
                Tab {
                    id: TabId(1),
                    name: "main".into(),
                    layout: LayoutTree::new(PaneId(1)),
                    focused: PaneId(1),
                    zoomed: None,
                },
                Tab {
                    id: TabId(2),
                    name: "other".into(),
                    layout: LayoutTree::new(PaneId(2)),
                    focused: PaneId(2),
                    zoomed: None,
                },
            ],
            Some(TabId(2)),
        )
        .expect("tabs");
    let frame = Compositor::default().compose(&state, &Overlay::empty(), None, 6, 40);
    let status = row_text(&frame.buffer, 5);
    assert!(status.contains("main [other]"), "{status:?}");
    assert!(!status.contains("[main]"), "{status:?}");
}

#[test]
fn terminal_hides_cursor_when_frame_transitions_from_some_to_none() {
    let mut state = workspace();
    let mut terminal = WorkspaceTerminal::enter(CaptureBackend::new(6, 12), false).expect("enter");
    terminal
        .render(&state, &Overlay::empty(), None)
        .expect("visible cursor");
    state
        .update_pane(PaneId(1), |pane| pane.cursor.hidden = true)
        .expect("hide cursor");
    terminal.backend_mut().bytes.clear();
    terminal
        .render(&state, &Overlay::empty(), None)
        .expect("hidden cursor");
    assert!(
        terminal
            .backend()
            .bytes
            .windows(6)
            .any(|bytes| bytes == b"\x1b[?25l")
    );
}

#[test]
fn wide_glyph_at_content_margin_is_clipped_but_one_column_inside_fits() {
    let mut state = workspace();
    state
        .update_pane(PaneId(1), |pane| {
            pane.cells.fill(Cell::default());
            pane.cells[9] = Cell {
                text: "界".into(),
                kind: CellKind::WideLeading,
                style: CellStyle::default(),
            };
            pane.cells[10] = Cell {
                text: String::new(),
                kind: CellKind::WideContinuation,
                style: CellStyle::default(),
            };
        })
        .expect("wide fit");
    let frame = Compositor::default().compose(&state, &Overlay::empty(), None, 6, 13);
    assert_eq!(
        frame.buffer.cell((10, 1)).map(|cell| cell.symbol()),
        Some("界")
    );
    state
        .update_pane(PaneId(1), |pane| {
            pane.cells.fill(Cell::default());
            pane.cells[10] = Cell {
                text: "界".into(),
                kind: CellKind::WideLeading,
                style: CellStyle::default(),
            };
            pane.cells[11] = Cell {
                text: String::new(),
                kind: CellKind::WideContinuation,
                style: CellStyle::default(),
            };
        })
        .expect("wide margin");
    let clipped = Compositor::default().compose(&state, &Overlay::empty(), None, 6, 13);
    assert_ne!(
        clipped.buffer.cell((11, 1)).map(|cell| cell.symbol()),
        Some("界")
    );
}

fn workspace() -> WorkspaceState {
    let mut state = WorkspaceState::default();
    state
        .insert_pane(PaneId(1), text_pane(4, 12, "hello"))
        .expect("pane");
    state
        .replace_tabs(
            vec![Tab {
                id: TabId(1),
                name: "main".into(),
                layout: LayoutTree::new(PaneId(1)),
                focused: PaneId(1),
                zoomed: None,
            }],
            Some(TabId(1)),
        )
        .expect("tab");
    state
}

fn text_pane(rows: u16, columns: u16, text: &str) -> PaneView {
    let mut cells = vec![Cell::default(); usize::from(rows) * usize::from(columns)];
    for (cell, character) in cells.iter_mut().zip(text.chars()) {
        *cell = Cell {
            text: character.to_string(),
            kind: CellKind::Text,
            style: CellStyle::default(),
        };
    }
    PaneView {
        rows,
        columns,
        cells,
        cursor: Default::default(),
        modes: Default::default(),
        title: String::new(),
        agent: AgentStatus::default(),
        exit_status: None,
        viewport_offset: 0,
        wrapped_rows: vec![false; usize::from(rows)],
        copy: Default::default(),
    }
}

fn row_text(buffer: &ratatui_core::buffer::Buffer, row: u16) -> String {
    (0..buffer.area.width)
        .filter_map(|column| buffer.cell((column, row)))
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn viewer_shortcuts_follow_workspace_bindings_and_reload_preserves_partial_input() {
    use fux::commands::{ClientBindings, Command};
    let bindings = [(b'q', Command::Detach), (b'w', Command::WorkspacePicker)];
    let policy = ClientBindings::new(2, bindings.iter().map(|(key, command)| (*key, command)));
    let mut filter = DetachFilter::new(vec![1]).expect("filter");
    filter.set_workspace_picker_enabled(true);
    assert!(filter.process(b"\x01", false).is_empty());
    assert_eq!(filter.configure(policy.clone()), b"\x01");
    // Old shortcuts and new-prefix ordinary keys reach the workspace unchanged.
    assert_eq!(
        filter.process(b"\x01d\x02d\x02s", false),
        b"\x01d\x02d\x02s"
    );
    assert!(filter.process(b"\x02q", false).is_empty());
    assert!(filter.take_detach());
    assert!(filter.process(b"\x02w", false).is_empty());
    assert!(filter.take_workspace_picker());
    assert_eq!(filter.process(b"\x02q\x02w", true), b"\x02q\x02w");
    assert!(!filter.take_workspace_picker());
    assert!(filter.process(b"\x02", false).is_empty());
    assert!(filter.configure(policy).is_empty());
    assert!(filter.process(b"q", false).is_empty());
    assert!(filter.take_detach());
    // Removing every binding also removes client-side interception.
    assert!(
        filter
            .configure(ClientBindings::new(2, std::iter::empty()))
            .is_empty()
    );
    assert_eq!(filter.process(b"\x02q\x02w", false), b"\x02q\x02w");
}

#[test]
fn contextual_prefix_holds_unknown_keys_and_cancels_without_leaking_escape_tails() {
    let mut filter = DetachFilter::new(vec![1]).expect("filter");
    filter.enable_contextual_help();
    assert!(filter.process_terminal_input(b"\x01").is_empty());
    assert!(filter.command_pending());
    assert!(!filter.hints_requested());
    assert!(filter.process_terminal_input(b"!").is_empty());
    assert!(filter.hints_requested());
    assert!(filter.process_terminal_input(b"\x1b").is_empty());
    assert!(filter.escape_pending());
    assert!(filter.process_terminal_input(b"[A").is_empty());
    assert!(filter.command_pending());
    assert!(filter.process_terminal_input(b"\x1b").is_empty());
    assert!(filter.resolve_escape().is_empty());
    assert!(!filter.command_pending());
    assert_eq!(filter.process_terminal_input(b"plain"), b"plain");
}

#[test]
fn contextual_prefix_uses_live_bindings_and_keeps_paste_and_literal_prefix_exact() {
    use fux::commands::{ClientBindings, Command};
    let mut filter = DetachFilter::new(vec![1]).expect("filter");
    filter.enable_contextual_help();
    let map = [(b'v', Command::SplitVertical), (b'q', Command::Detach)];
    let policy = ClientBindings::new(2, map.iter().map(|(key, value)| (*key, value)));
    assert_eq!(
        policy.entries().collect::<Vec<_>>(),
        [(b'q', "detach viewer"), (b'v', "split stacked")]
    );
    assert!(filter.configure(policy).is_empty());
    assert!(filter.process_terminal_input(b"\x02v").is_empty());
    assert_eq!(
        filter.take_viewer_action(),
        Some(fux::commands::BuiltinAction::SplitVertical)
    );
    assert!(!filter.command_pending());
    assert_eq!(filter.process_terminal_input(b"\x02\x02"), b"\x02");
    let paste = b"\x1b[200~\x02q\x1b[201~";
    let output: Vec<_> = paste
        .iter()
        .flat_map(|byte| filter.process_terminal_input(&[*byte]))
        .collect();
    assert_eq!(output, paste);
    assert!(!filter.take_detach());
    assert!(filter.process_terminal_input(b"\x02q").is_empty());
    assert!(filter.take_detach());
}

#[test]
fn contextual_partial_escape_timeout_does_not_trap_command_mode_or_leak_a_prefix() {
    let mut filter = DetachFilter::new(vec![1]).expect("filter");
    filter.enable_contextual_help();
    assert!(filter.process_terminal_input(b"\x01\x1b[").is_empty());
    assert!(!filter.escape_pending());
    assert!(filter.resolve_escape().is_empty());
    assert!(filter.command_pending());
    assert!(filter.process_terminal_input(b"\x1b").is_empty());
    assert!(filter.resolve_escape().is_empty());
    assert!(!filter.command_pending());
    assert!(filter.process_terminal_input(b"\x01").is_empty());
    assert!(
        filter.flush().is_empty(),
        "EOF must not leave shared server prefix state"
    );
}

#[test]
fn viewer_rename_and_confirmation_consume_fragmented_input_without_mutating_the_snapshot() {
    use fux::client::interaction::Interaction;
    use fux::commands::BuiltinAction;
    use fux::control::{Request, TabAction};
    let state = workspace();
    let before = state.clone();
    let mut interaction = Interaction::default();
    interaction.enter(BuiltinAction::RenameTab, &state);
    for byte in b"\x15".iter().chain("new界".as_bytes()) {
        assert!(interaction.feed(*byte, &state).is_none());
    }
    let request = interaction.feed(b'\r', &state).expect("submit rename");
    assert_eq!(
        request,
        Request::Tab {
            id: 0,
            action: TabAction::Rename {
                tab: 1,
                name: "new界".into()
            }
        }
    );
    assert!(!interaction.active());
    assert_eq!(
        state, before,
        "viewer editing must not mutate authoritative state"
    );
    interaction.enter(BuiltinAction::ClosePane, &state);
    for byte in b"\x1b[200~y\x1b[201~" {
        assert!(interaction.feed(*byte, &state).is_none());
    }
    assert!(
        interaction.active(),
        "pasted y must not confirm destructive action"
    );
    assert_eq!(
        interaction.feed(b'y', &state),
        Some(Request::Kill { id: 0, pane: 1 })
    );
}

#[test]
fn workspace_picker_replays_fragmented_loading_input_and_ignores_canceled_results() {
    use fux::client::interaction::Interaction;
    let state = workspace();
    let mut interaction = Interaction::default();
    interaction.loading_workspaces();
    for byte in b"\x1b[" {
        assert!(interaction.feed(*byte, &state).is_none());
    }
    interaction.workspaces_loaded(Ok(vec!["alpha".into(), "beta".into()]));
    let buffered = interaction.take_loading_input();
    for byte in buffered.iter().chain(b"B\r") {
        assert!(interaction.feed(*byte, &state).is_none());
    }
    assert_eq!(interaction.take_workspace().as_deref(), Some("beta"));
    assert!(!interaction.active());

    interaction.loading_workspaces();
    interaction.feed(27, &state);
    interaction.resolve_escape();
    assert!(interaction.take_back());
    interaction.workspaces_loaded(Ok(vec!["alpha".into()]));
    assert!(
        !interaction.active(),
        "late lookup must not reopen canceled picker"
    );
    assert!(interaction.take_workspace().is_none());
}

#[test]
fn copy_interactions_keep_selection_and_clipboard_private_and_escape_one_level() {
    use fux::client::interaction::Interaction;
    use fux::commands::BuiltinAction;
    let state = workspace();
    let before = state.clone();
    let mut first = Interaction::default();
    let second = Interaction::default();
    first.enter(BuiltinAction::CopyMode, &state);
    assert_eq!(first.take_copy_read(), Some((1, 0)));
    for byte in b" llll" {
        first.feed(*byte, &state);
    }
    assert!(
        first
            .copy_ui()
            .view
            .as_ref()
            .unwrap()
            .1
            .copy
            .anchor
            .is_some()
    );
    assert!(second.copy_ui().view.is_none());
    first.feed(27, &state);
    first.resolve_escape();
    assert!(first.active(), "first Esc should clear selection");
    assert!(
        first
            .copy_ui()
            .view
            .as_ref()
            .unwrap()
            .1
            .copy
            .anchor
            .is_none()
    );
    assert!(!first.take_back());
    for byte in b"hhhh lllly" {
        first.feed(*byte, &state);
    }
    assert!(!first.active());
    let ui = first.copy_ui();
    assert!(ui.view.is_none());
    assert_eq!(
        ui.clipboard.as_ref().map(|(_, text)| text.as_str()),
        Some("aGVsbG8=")
    );
    assert!(second.copy_ui().clipboard.is_none());
    assert_eq!(state, before);
    for enabled in [false, true] {
        let (_, receiver) = tokio::sync::watch::channel(ui.clone());
        let mut terminal = WorkspaceTerminal::enter(CaptureBackend::new(6, 20), enabled)
            .unwrap()
            .with_copy_ui(receiver);
        terminal.render(&state, &Overlay::empty(), None).unwrap();
        terminal.render(&state, &Overlay::empty(), None).unwrap();
        let marker = b"\x1b]52;c;aGVsbG8=";
        assert_eq!(
            terminal
                .backend()
                .bytes
                .windows(marker.len())
                .filter(|window| *window == marker)
                .count(),
            usize::from(enabled)
        );
    }
    first.enter(BuiltinAction::CopyMode, &state);
    first.feed(27, &state);
    first.resolve_escape();
    assert!(!first.active());
    assert!(
        first.take_back(),
        "Esc without a selection returns to commands"
    );
}

#[test]
fn copy_view_stays_on_its_target_and_refreshes_after_resize_without_shared_mutation() {
    use fux::client::interaction::Interaction;
    use fux::commands::BuiltinAction;
    let mut state = workspace();
    let mut interaction = Interaction::default();
    interaction.enter(BuiltinAction::CopyMode, &state);
    interaction.take_copy_read();
    state
        .insert_pane(PaneId(2), text_pane(4, 12, "second"))
        .unwrap();
    let mut tabs = state.tabs().to_vec();
    tabs.push(Tab {
        id: TabId(2),
        name: "second".into(),
        layout: LayoutTree::new(PaneId(2)),
        focused: PaneId(2),
        zoomed: None,
    });
    state.replace_tabs(tabs, Some(TabId(2))).unwrap();
    let before = state.clone();
    let private = interaction.copy_ui().apply(&state).unwrap();
    assert_eq!(private.active_tab(), Some(TabId(1)));
    assert_eq!(state, before);
    for byte in b"\x1b[200~ y\x1b[201~" {
        interaction.feed(*byte, &state);
    }
    assert!(
        interaction.copy_ui().clipboard.is_none(),
        "pasted copy keys must not execute"
    );
    interaction.feed(b' ', &state);
    state
        .update_pane(PaneId(1), |pane| *pane = text_pane(2, 8, "resized"))
        .unwrap();
    interaction.reconcile_copy(&state);
    assert_eq!(interaction.take_copy_read(), Some((1, 0)));
    interaction.install_copy_view(fux::local::CopyViewReply {
        request: 1,
        pane: 1,
        view: state.pane(PaneId(1)).cloned().map(Box::new),
    });
    let view = interaction.copy_ui().view.unwrap().1;
    assert_eq!((view.rows, view.columns), (2, 8));
    assert!(view.copy.anchor.is_none());
    let tabs = state
        .tabs()
        .iter()
        .filter(|tab| tab.id != TabId(1))
        .cloned()
        .collect();
    state.replace_tabs(tabs, Some(TabId(2))).unwrap();
    state.remove_pane(PaneId(1)).unwrap();
    interaction.reconcile_copy(&state);
    assert!(!interaction.active());
    assert!(interaction.take_back());
}

#[test]
fn viewer_mouse_decoder_preserves_fragmented_reports_and_pasted_bytes() {
    let report = b"\x1b[<064;02;03M";
    for boundary in 0..=report.len() {
        let mut filter = DetachFilter::new(vec![1]).unwrap();
        filter.enable_contextual_help();
        assert!(
            filter
                .process_terminal_input(&report[..boundary])
                .is_empty()
        );
        let mut event = filter.take_mouse();
        assert!(
            filter
                .process_terminal_input(&report[boundary..])
                .is_empty()
        );
        event = event.or_else(|| filter.take_mouse());
        let (mouse, raw) = event.expect("complete mouse report");
        assert!(mouse.wheel());
        assert_eq!((mouse.column, mouse.row), (2, 3));
        assert_eq!(
            raw, report,
            "application fallback must preserve original bytes"
        );
    }
    let mut filter = DetachFilter::new(vec![1]).unwrap();
    filter.enable_contextual_help();
    let paste = b"\x1b[200~\x1b[<4;2;3M\x1b[201~";
    assert_eq!(filter.process_terminal_input(paste), paste);
    assert!(filter.take_mouse().is_none());
}

#[test]
fn mouse_selection_and_scrolling_are_local_and_application_mouse_stays_available() {
    use fux::client::interaction::Interaction;
    use fux::host::MouseEvent;
    let mut state = workspace();
    let before = state.clone();
    let mut viewer = Interaction::default();
    viewer.set_mouse_layout(vec![(
        PaneId(1),
        ratatui_core::layout::Rect::new(0, 0, 14, 6),
    )]);
    for (code, column, release) in [(4, 2, false), (36, 6, false), (4, 6, true)] {
        assert!(viewer.mouse(
            MouseEvent {
                code,
                column,
                row: 2,
                release
            },
            &state
        ));
    }
    viewer.feed(b'y', &state);
    assert_eq!(viewer.copy_ui().clipboard.unwrap().1, "aGVsbG8=");
    assert_eq!(state, before);
    let wheel = MouseEvent {
        code: 64,
        column: 2,
        row: 2,
        release: false,
    };
    state
        .update_pane(PaneId(1), |pane| {
            pane.modes.mouse_mode = fux::state::MouseMode::AnyMotion
        })
        .unwrap();
    assert!(
        !viewer.mouse(wheel, &state),
        "application mouse wheel was intercepted"
    );
    state
        .update_pane(PaneId(1), |pane| {
            pane.modes.mouse_mode = fux::state::MouseMode::None
        })
        .unwrap();
    assert!(viewer.mouse(wheel, &state));
    assert_eq!(viewer.take_copy_read(), Some((1, 3)));
    assert_eq!(state.pane(PaneId(1)).unwrap().viewport_offset, 0);
    assert!(!state.pane(PaneId(1)).unwrap().copy.active);
}

#[test]
fn popup_copy_highlight_and_mouse_geometry_stay_inside_the_visible_popup() {
    let mut state = workspace();
    state
        .insert_pane(PaneId(2), text_pane(3, 8, "popup"))
        .unwrap();
    state
        .replace_popups(vec![Popup {
            pane: PaneId(2),
            width: 10,
            height: 5,
            z_index: 1,
        }])
        .unwrap();
    let plain = Compositor::default().compose(&state, &Overlay::empty(), None, 10, 30);
    state
        .update_pane(PaneId(2), |pane| {
            pane.copy.active = true;
            pane.copy.anchor = Some((0, 0));
            pane.copy.cursor_row = 2;
            pane.copy.cursor_column = 1;
        })
        .unwrap();
    let selected = Compositor::default().compose(&state, &Overlay::empty(), None, 10, 30);
    let rect = selected.pane_rects[&PaneId(2)];
    let content =
        ratatui_core::layout::Rect::new(rect.x + 1, rect.y + 1, rect.width - 2, rect.height - 2);
    for y in 0..10 {
        for x in 0..30 {
            if !content.contains((x, y).into()) {
                assert_eq!(
                    selected.buffer.cell((x, y)),
                    plain.buffer.cell((x, y)),
                    "selection escaped popup at {x},{y}"
                );
            }
        }
    }
    assert!(
        selected
            .buffer
            .cell((content.x, content.y))
            .unwrap()
            .modifier
            .contains(ratatui_core::style::Modifier::REVERSED)
    );
    let (sender, receiver) = tokio::sync::watch::channel(Vec::new());
    let mut terminal = WorkspaceTerminal::enter(CaptureBackend::new(10, 30), false)
        .unwrap()
        .with_mouse_layout(sender);
    terminal.render(&state, &Overlay::empty(), None).unwrap();
    assert_eq!(
        *receiver.borrow(),
        vec![(PaneId(2), rect)],
        "mouse hit testing exposed covered panes"
    );
}

#[test]
fn popup_footer_uses_configured_prefix_and_close_binding() {
    use fux::client::hints::HintPanel;
    use fux::commands::{ClientBindings, Command};
    let bindings = [(b'v', Command::Close)];
    let policy = ClientBindings::new(2, bindings.iter().map(|(key, command)| (*key, command)));
    let mut buffer =
        ratatui_core::buffer::Buffer::empty(ratatui_core::layout::Rect::new(0, 0, 80, 4));
    HintPanel::popup(&policy).paint(&mut buffer);
    let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
    assert!(text.contains("C-b commands"));
    assert!(text.contains("C-b v close (confirm)"));
    assert!(!text.contains("C-a"));
}

#[test]
fn command_groups_and_context_restrictions_share_the_dispatch_registry() {
    use fux::commands::{BuiltinAction as A, DEFAULT_BINDINGS};
    let mut state = workspace();
    for spec in DEFAULT_BINDINGS {
        assert_eq!(spec.command.group(), spec.action.group());
    }
    assert!(A::ResizeMode.unavailable(&state, true).is_some());
    assert!(A::NextTab.unavailable(&state, true).is_some());
    assert!(A::WorkspacePicker.unavailable(&state, false).is_some());
    assert!(A::RenameTab.unavailable(&state, true).is_none());
    state
        .insert_pane(PaneId(2), text_pane(4, 12, "second"))
        .unwrap();
    let mut tabs = state.tabs().to_vec();
    tabs[0]
        .layout
        .split(
            PaneId(1),
            PaneId(2),
            fux::state::Axis::Horizontal,
            std::num::NonZeroU16::new(5000).unwrap(),
        )
        .unwrap();
    state.replace_tabs(tabs, Some(TabId(1))).unwrap();
    assert!(A::ResizeMode.unavailable(&state, true).is_none());
    state
        .insert_pane(PaneId(3), text_pane(2, 8, "popup"))
        .unwrap();
    state
        .replace_popups(vec![Popup {
            pane: PaneId(3),
            width: 10,
            height: 4,
            z_index: 1,
        }])
        .unwrap();
    assert!(A::ResizeMode.unavailable(&state, true).is_some());
    assert!(A::NewTab.unavailable(&state, true).is_some());
    assert!(A::ClosePane.unavailable(&state, true).is_none());
    assert!(A::CopyMode.unavailable(&state, true).is_none());
}

#[test]
fn grouped_command_pages_expose_every_binding_and_dim_unavailable_actions() {
    use fux::client::hints::HintPanel;
    use fux::commands::{ClientBindings, Command, key_name};
    let state = workspace();
    let mut commands: Vec<_> = fux::commands::DEFAULT_BINDINGS
        .iter()
        .map(|spec| (spec.key, spec.command.clone()))
        .collect();
    commands.push((b'e', Command::External(vec!["private-command".into()])));
    let bindings = ClientBindings::new(1, commands.iter().map(|(key, command)| (*key, command)));
    let mut all = String::new();
    let mut dimmed_resize = false;
    for page in 0..40 {
        let mut buffer =
            ratatui_core::buffer::Buffer::empty(ratatui_core::layout::Rect::new(0, 0, 80, 5));
        for row in 0..5 {
            for column in 0..80 {
                buffer.cell_mut((column, row)).unwrap().modifier =
                    ratatui_core::style::Modifier::DIM | ratatui_core::style::Modifier::ITALIC;
            }
        }
        HintPanel::commands(&bindings, false, page, &state).paint(&mut buffer);
        for row in 0..5 {
            let line: String = (0..80)
                .map(|column| buffer.cell((column, row)).unwrap().symbol())
                .collect();
            if line.starts_with("x  close pane") {
                assert!(
                    !buffer.cell((0, row)).unwrap().modifier.intersects(
                        ratatui_core::style::Modifier::DIM | ratatui_core::style::Modifier::ITALIC
                    ),
                    "available action inherited pane styling"
                );
            }
            if line.starts_with("r  resize mode") {
                dimmed_resize |= buffer
                    .cell((0, row))
                    .unwrap()
                    .modifier
                    .contains(ratatui_core::style::Modifier::DIM);
            }
            all.push_str(&line);
            all.push('\n');
        }
    }
    for (key, description) in bindings.entries() {
        assert!(
            all.contains(&format!("{}  {description}", key_name(key))),
            "binding {key} unreachable"
        );
    }
    for group in ["Panes", "Focus", "Tabs", "Session", "Custom"] {
        assert!(all.contains(group));
    }
    assert!(dimmed_resize);
    assert!(
        !all.contains("private-command"),
        "external argv leaked into hints"
    );
    for width in 0..4 {
        for height in 0..4 {
            let mut buffer = ratatui_core::buffer::Buffer::empty(ratatui_core::layout::Rect::new(
                3, 2, width, height,
            ));
            HintPanel::commands(&bindings, true, usize::MAX, &state).paint(&mut buffer);
        }
    }
}

#[test]
fn unavailable_workspace_shortcut_reaches_context_feedback_without_forwarding() {
    let mut filter = DetachFilter::new(vec![1]).unwrap();
    filter.enable_contextual_help();
    filter.set_workspace_picker_enabled(false);
    assert!(filter.process_terminal_input(b"\x01s").is_empty());
    let action = filter.take_viewer_action().expect("contextual action");
    assert_eq!(action, fux::commands::BuiltinAction::WorkspacePicker);
    assert!(
        action
            .unavailable(&workspace(), false)
            .unwrap()
            .contains("No workspace manager")
    );
    assert!(!filter.take_workspace_picker());
}

#[test]
fn contextual_external_commands_and_literal_prefix_have_distinct_output_paths() {
    use fux::commands::{ClientBindings, Command};
    let commands = [(b'e', Command::External(vec!["configured-only".into()]))];
    let mut filter = DetachFilter::new(vec![1]).unwrap();
    filter.enable_contextual_help();
    filter.configure(ClientBindings::new(
        1,
        commands.iter().map(|(key, command)| (*key, command)),
    ));
    assert_eq!(filter.process_terminal_input(b"\x01\x01"), b"\x01");
    assert!(filter.take_external_binding().is_none());
    assert!(filter.process_terminal_input(b"\x01e").is_empty());
    assert_eq!(filter.take_external_binding(), Some(b'e'));
    assert_eq!(filter.process_terminal_input(b"e"), b"e");
    assert!(filter.take_external_binding().is_none());
}

#[test]
fn paste_delimiters_cannot_execute_a_matching_configured_prefix() {
    use fux::commands::{ClientBindings, Command};
    let bindings = [(b'[', Command::CopyMode), (b'x', Command::Close)];
    for prefix in [27, b'2'] {
        let mut filter = DetachFilter::new(vec![prefix]).unwrap();
        filter.enable_contextual_help();
        filter.configure(ClientBindings::new(
            prefix,
            bindings.iter().map(|(key, command)| (*key, command)),
        ));
        let paste = b"\x1b[200~ordinary pasted bytes\x1b[201~";
        let mut output = Vec::new();
        for byte in paste {
            output.extend(filter.process_terminal_input(&[*byte]));
        }
        assert_eq!(output, paste);
        assert!(!filter.command_pending());
        assert!(
            filter.take_viewer_action().is_none(),
            "paste delimiter became a copy command"
        );
    }
}

#[test]
fn terminal_escape_parameters_do_not_activate_printable_prefixes() {
    use fux::commands::{ClientBindings, Command};
    let bindings = [(b'x', Command::Close)];
    for prefix in [b'A', b'2', b'P'] {
        for sequence in [b"\x1b[A".as_slice(), b"\x1b[1;2A", b"\x1bOP"] {
            let mut filter = DetachFilter::new(vec![prefix]).unwrap();
            filter.enable_contextual_help();
            filter.configure(ClientBindings::new(
                prefix,
                bindings.iter().map(|(key, command)| (*key, command)),
            ));
            let mut output = Vec::new();
            for byte in sequence {
                output.extend(filter.process_terminal_input(&[*byte]));
                if !filter.escape_pending() {
                    output.extend(filter.resolve_escape());
                }
            }
            assert_eq!(output, sequence);
            assert!(!filter.command_pending());
            assert!(filter.take_viewer_action().is_none());
        }
    }
}

#[test]
fn partial_terminal_reports_and_paste_delimiters_survive_escape_timeout() {
    use fux::commands::{ClientBindings, Command};
    let bindings = [(b'x', Command::Close)];
    for sequence in [
        b"\x1b[1;2A".as_slice(),
        b"\x1b[<0;2;1M",
        b"\x1b[200~2x\x1b[201~",
    ] {
        let mut filter = DetachFilter::new(vec![b'2']).unwrap();
        filter.enable_contextual_help();
        filter.configure(ClientBindings::new(
            b'2',
            bindings.iter().map(|(key, command)| (*key, command)),
        ));
        let mut output = filter.process_terminal_input(&sequence[..2]);
        assert!(!filter.escape_pending());
        output.extend(filter.resolve_escape());
        for byte in &sequence[2..] {
            output.extend(filter.process_terminal_input(&[*byte]));
            if let Some((_, bytes)) = filter.take_mouse() {
                output.extend(bytes);
            }
        }
        assert_eq!(output, sequence);
        assert!(!filter.command_pending());
        assert!(filter.take_viewer_action().is_none());
    }
    let mut filter = DetachFilter::new(vec![b'2']).unwrap();
    filter.enable_contextual_help();
    assert!(filter.process_terminal_input(b"\x1b[2").is_empty());
    assert_eq!(filter.flush(), b"\x1b[2");
    assert!(!filter.command_pending());
}

#[test]
fn oversized_copy_preserves_selection_and_can_retry_a_smaller_region() {
    use fux::client::copy::CopySession;
    let mut pane = text_pane(512, 512, "");
    for cell in &mut pane.cells {
        cell.text = "a\u{301}".into();
        cell.kind = CellKind::Text;
    }
    let mut copy = CopySession::new(PaneId(1), pane).expect("valid maximum-size pane");
    copy.mouse(0, 0, false);
    copy.mouse(511, 511, true);
    copy.key('y');
    assert!(copy.active());
    assert!(copy.selecting());
    assert!(copy.clipboard().is_empty());
    assert!(
        copy.error()
            .is_some_and(|error| error.contains("exceeds clipboard limit"))
    );
    assert!(copy.escape());
    assert!(copy.error().is_none());
    copy.mouse(0, 0, false);
    copy.mouse(0, 0, true);
    copy.key('y');
    assert!(!copy.active());
    assert_eq!(copy.clipboard(), "YcyB");
}

#[test]
fn delayed_paste_delimiter_cannot_confirm_a_destructive_dialog() {
    use fux::client::interaction::Interaction;
    use fux::commands::BuiltinAction;
    let state = workspace();
    let mut interaction = Interaction::default();
    interaction.enter(BuiltinAction::ClosePane, &state);
    for byte in b"\x1b[20" {
        assert!(interaction.feed(*byte, &state).is_none());
    }
    assert!(!interaction.escape_pending());
    interaction.resolve_escape();
    for byte in b"0~y\r\x1b[201~" {
        assert!(interaction.feed(*byte, &state).is_none());
    }
    assert!(interaction.active());
    assert!(
        interaction.feed(b'y', &state).is_some(),
        "only an explicit unpasted confirmation executes"
    );
}

#[test]
fn delayed_paste_delimiter_cannot_dispatch_a_prefix_command() {
    let mut filter = DetachFilter::new(vec![1]).unwrap();
    filter.enable_contextual_help();
    assert!(filter.process_terminal_input(b"\x01\x1b[20").is_empty());
    assert!(!filter.escape_pending());
    assert!(filter.resolve_escape().is_empty());
    let output = filter.process_terminal_input(b"0~\x01x\x1b[201~");
    assert_eq!(output, b"\x1b[200~\x01x\x1b[201~");
    assert!(!filter.command_pending());
    assert!(filter.take_viewer_action().is_none());
}

#[test]
fn rename_input_keeps_the_grapheme_and_insertion_point_visible_when_narrow() {
    use fux::client::hints::HintPanel;
    use ratatui_core::{buffer::Buffer, layout::Rect};
    let panel = HintPanel::text_input("Rename", "long prefix 界a\u{301}", "Enter save");
    for width in [1, 2, 4, 20] {
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, 3));
        panel.paint(&mut buffer);
        let text: String = (0..width)
            .map(|column| buffer[(column, 1)].symbol())
            .collect();
        assert!(text.contains('▏'));
        if width >= 2 {
            assert!(text.contains("a\u{301}"));
        }
        if width == 4 {
            assert!(text.starts_with("界"));
        }
    }
}
