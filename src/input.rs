//! Routing a client's input: to its overlay, its copy mode, the command
//! column, or its focused pane.
use crate::command::ClientId;
use crate::decode::Input;
use crate::keys::KeyPress;
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
        for input in inputs {
            if !self.views.contains_key(&client) {
                break;
            }
            match input {
                Input::Key(press) => self.key(client, press),
                Input::Paste(text) => self.paste(client, &text),
                Input::PasteTooLong => self.error_to(client, "paste exceeds 64 KiB; discarded"),
                Input::FocusIn | Input::FocusOut => {
                    self.focus_event(client, input == Input::FocusIn)
                }
            }
        }
        self.touch();
        self.settle();
    }

    fn key(&mut self, client: ClientId, press: KeyPress) {
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
                } else {
                    overlay::send_key(self, client, press);
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
}
