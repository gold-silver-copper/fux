//! Routing a client's input: to its overlay, its copy mode, the command
//! column, or its focused pane.
use crate::command::ClientId;
use crate::decode::Input;
use crate::keys::Keystroke;
use crate::keys::mouse::{MouseAction, MouseEvent};
use crate::layout::Placement;
use crate::overlay;
use crate::session::Session;
use crate::view::Mode;
use std::time::Instant;

/// How much of a client's input is decoded and dispatched at once.
const INPUT_PIECE: usize = 4096;

impl Session {
    /// Raw bytes from a client's terminal, now.
    pub fn input(&mut self, client: ClientId, bytes: &[u8]) {
        self.input_at(client, bytes, Instant::now());
    }

    /// Raw bytes from a client's terminal, read at `now`: an Escape they
    /// leave waiting is due `ESCAPE_DELAY` after it.
    ///
    /// The bytes are decoded and dispatched `INPUT_PIECE` at a time, so a
    /// large frame of input never becomes a key for every byte at once.
    pub fn input_at(&mut self, client: ClientId, bytes: &[u8], now: Instant) {
        let mut inputs = Vec::new();
        let mut rest = bytes;
        while !rest.is_empty() {
            let (piece, later) = rest
                .split_at_checked(INPUT_PIECE.min(rest.len()))
                .unwrap_or((rest, &[]));
            rest = later;
            match self.views.get_mut(&client) {
                Some(view) => {
                    view.decoder.expire(now);
                    view.decoder.bytes(piece, &mut inputs);
                    view.decoder.mark(now);
                }
                None => return,
            }
            self.dispatch(client, std::mem::take(&mut inputs));
        }
    }

    /// The first client, in their order, whose Escape was due by `now`.
    pub fn escape_due(&self, now: Instant) -> Option<ClientId> {
        self.views
            .iter()
            .find(|(_, v)| v.decoder.deadline().is_some_and(|due| due <= now))
            .map(|(client, _)| *client)
    }

    /// The Escape deadline passed for a client.
    pub fn escape(&mut self, client: ClientId) {
        let mut inputs = Vec::new();
        match self.views.get_mut(&client) {
            Some(view) => view.decoder.timeout(&mut inputs),
            None => return,
        }
        self.dispatch(client, inputs);
    }

    fn dispatch(&mut self, client: ClientId, inputs: Vec<Input>) {
        if inputs.is_empty() {
            return;
        }
        let changes = self.changes();
        for input in inputs {
            if !self.views.contains_key(&client) {
                break;
            }
            match input {
                Input::Key(stroke) => self.key(client, stroke),
                Input::Paste(text) => self.paste(client, &text),
                Input::PasteTooLong => self.error_to(client, "paste exceeds 64 KiB; discarded"),
                Input::FocusIn | Input::FocusOut => {
                    self.focus_event(client, input == Input::FocusIn)
                }
                Input::Reply(reply) => self.terminal_reply(client, reply),
                Input::Mouse(event) => self.mouse(client, event),
            }
        }
        // A command the input ran repainted every client already; else only
        // this client's screen changed: its mode, notice or overlay. A key
        // a pane's program reads shows when its output does.
        if self.changes() == changes
            && let Some(view) = self.views.get_mut(&client)
        {
            view.dirty = true;
        }
        self.settle();
    }

    /// A key: matched as its press, and given to a pane as it was typed.
    fn key(&mut self, client: ClientId, stroke: Keystroke) {
        let press = stroke.press;
        let Some(view) = self.views.get_mut(&client) else {
            return;
        };
        view.dirty = true;
        match &view.mode {
            Mode::Copy(_) => crate::copy::key(self, client, press),
            Mode::List(_) => overlay::list_key(self, client, press),
            Mode::Prompt(_) => overlay::prompt_key(self, client, press),
            Mode::Confirm(_) => overlay::confirm_key(self, client, press),
            Mode::Column { .. } => overlay::column_key(self, client, press),
            Mode::Repeat { .. } => overlay::repeat_key(self, client, press),
            Mode::Normal => {
                // Further input clears the last notice.
                view.notice = None;
                if press == self.config.prefix {
                    view.mode = Mode::Column {
                        path: Vec::new(),
                        selected: 0,
                    };
                } else if !overlay::run_root(self, client, press) {
                    overlay::send_key(self, client, stroke);
                }
            }
        }
    }

    fn paste(&mut self, client: ClientId, text: &str) {
        let Some(view) = self.views.get_mut(&client) else {
            return;
        };
        if let Mode::Prompt(_) = view.mode {
            return overlay::prompt_paste(self, client, text);
        }
        // Pastes never become commands.
        if !matches!(view.mode, Mode::Normal) {
            return;
        }
        view.notice = None;
        let Some(pane) = view.focus() else { return };
        self.typed(client);
        let Some(p) = self.panes.get_mut(&pane) else {
            return;
        };
        let bracketed = p.screen().bracketed_paste();
        if let Err(error) = p
            .input
            .push_with(|out| crate::encode::paste(text, bracketed, out))
        {
            self.error_to(client, error.to_string());
        }
    }

    /// The outer terminal gained or lost focus: the client's focused pane
    /// hears of it, if it asked.
    fn focus_event(&mut self, client: ClientId, gained: bool) {
        let Some(pane) = self.views.get(&client).and_then(|v| v.focus()) else {
            return;
        };
        if let Some(p) = self.panes.get_mut(&pane)
            && p.screen().focus_reporting()
        {
            let _ = p.input.push(if gained { b"\x1b[I" } else { b"\x1b[O" });
        }
    }

    /// A mouse report from the client's terminal, which reports only while
    /// its focused pane's program asked for it (`render::mouse_level`):
    /// moved to the pane's cells and encoded as the program asked. fux has
    /// no mouse actions of its own: a report outside the focused pane is
    /// dropped, except that the motion and release of a press made inside
    /// are kept to the pane's edge, so the program never sees a button left
    /// down. One that arrives as an overlay opens is dropped.
    fn mouse(&mut self, client: ClientId, event: MouseEvent) {
        let Some(view) = self.views.get(&client) else {
            return;
        };
        let Some(pane) = view.focus().filter(|_| matches!(view.mode, Mode::Normal)) else {
            return;
        };
        let mut placement = Placement::default();
        self.placement_into(view, &mut placement);
        let Some(rect) = placement.rect(pane) else {
            return;
        };
        let inside = rect.contains(event.col, event.row);
        let held = view.mouse_held == Some(pane);
        let pressed =
            event.action == MouseAction::Press && event.button.is_some_and(|b| !b.is_wheel());
        if !(inside || held && event.action != MouseAction::Press) {
            return;
        }
        if let Some(view) = self.views.get_mut(&client) {
            if pressed {
                view.mouse_held = Some(pane);
            } else if event.action == MouseAction::Release {
                view.mouse_held = None;
            }
        }
        // Kept to the pane: `rect` holds the press, so it is not empty.
        let last = |start: u16, len: u16| start.saturating_add(len.saturating_sub(1));
        let event = MouseEvent {
            row: event
                .row
                .clamp(rect.y, last(rect.y, rect.h))
                .saturating_sub(rect.y),
            col: event
                .col
                .clamp(rect.x, last(rect.x, rect.w))
                .saturating_sub(rect.x),
            ..event
        };
        self.typed(client);
        let Some(p) = self.panes.get_mut(&pane) else {
            return;
        };
        let mut report = Vec::new();
        if p.screen().encode_mouse(event, &mut report) && !p.input.refusing() {
            let _ = p.input.push(&report);
        }
    }
}
