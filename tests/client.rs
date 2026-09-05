#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

use fux::client::{
    CaptureBackend, ClientNotificationGate, Compositor, CopyMode, DetachFilter, Selection,
    WorkspaceTerminal, client_notification_command,
};
use fux::state::{
    AgentStatus, Cell, CellKind, CellStyle, LayoutTree, PaneId, PaneView, Popup, Tab, TabId,
    WorkspaceState,
};
use koh::client::backend::KohBackend;
use koh::client::{ClientState, ClientTerminal};
use koh::predict::{DisplayPreference, Overlay, PredictionEngine};

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
fn workspace_client_state_exposes_window_modes_ack_and_a_safe_prediction_shadow() {
    // Phase F3 ClientState: focused live panes drive modes and prediction unless scrolled.
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
            metadata.echo_ack = 9;
            metadata.exit_code = Some(7);
        })
        .expect("metadata");
    let window = state.window();
    assert_eq!(
        (window.title, window.clipboard, window.bell_count),
        ("work", "aGk=", 3)
    );
    assert_eq!((state.echo_ack(), state.exit_code()), (9, Some(7)));
    let modes = state.input_modes();
    assert!(modes.application_cursor && modes.application_keypad && modes.bracketed_paste);
    assert_eq!(modes.mouse_encoding, vt100::MouseProtocolEncoding::Sgr);
    assert!(state.predict_target().is_some());
    state
        .update_pane(PaneId(1), |pane| pane.viewport_offset = 1)
        .expect("scroll");
    assert!(state.predict_target().is_none());
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
    let pane = text_pane(4, 12, "");
    let mut predictor = PredictionEngine::new(DisplayPreference::Always);
    predictor.set_local_frame_sent(0);
    predictor.new_user_byte(1, b'x', &pane);
    let mut echoed = pane.clone();
    echoed.cells[0] = Cell {
        text: "x".into(),
        kind: CellKind::Text,
        style: CellStyle::default(),
    };
    echoed.cursor.column = 1;
    predictor.set_local_frame_late_acked(1);
    predictor.cull(2, &echoed);
    predictor.set_local_frame_sent(1);
    predictor.new_user_byte(3, b'y', &echoed);
    let overlay = predictor.overlay(&echoed);
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

impl KohBackend for FaultBackend {
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

impl KohBackend for EnterFaultBackend {
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
fn prefix_and_mouse_control_bytes_never_create_visible_prediction_glyphs() {
    // Phase F3 prediction: local control bytes may open epochs but never ghost into pane content.
    let pane = text_pane(4, 12, "");
    for bytes in [b"\x01".as_slice(), b"\x1b[<0;2;2M".as_slice()] {
        let mut predictor = PredictionEngine::new(DisplayPreference::Always);
        predictor.set_local_frame_sent(1);
        for &byte in bytes {
            predictor.new_user_byte(1, byte, &pane);
        }
        let overlay = predictor.overlay(&pane);
        for row in 0..pane.rows {
            for column in 0..pane.columns {
                assert!(
                    overlay.cell(row, column).is_none(),
                    "control byte produced a cell at {row},{column}"
                );
            }
        }
    }
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
