//! Routing a client's input: to its overlay, its copy mode, the command
//! column, or its focused pane.
use crate::command::ClientId;
use crate::decode::Input;
use crate::keys::KeyPress;
use crate::overlay;
use crate::session::Session;
use crate::view::Mode;

impl Session {
    /// Raw bytes from a client's terminal.
    pub fn input(&mut self, client: ClientId, bytes: &[u8]) {
        let mut inputs = Vec::new();
        match self.views.get_mut(&client) {
            Some(view) => view.decoder.bytes(bytes, &mut inputs),
            None => return,
        }
        self.dispatch(client, inputs);
    }

    /// Whether a client's decoder waits on the Escape deadline.
    pub fn waiting(&self, client: ClientId) -> bool {
        self.views.get(&client).is_some_and(|v| v.decoder.waiting())
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
                Input::PasteTooLong => {
                    if let Some(view) = self.views.get_mut(&client) {
                        view.error("paste exceeds 64 KiB; discarded");
                    }
                }
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
        match &view.mode {
            Mode::Prompt(_) => overlay::prompt_paste(self, client, text),
            // Pastes never become commands.
            Mode::Copy(_)
            | Mode::List(_)
            | Mode::Confirm(_)
            | Mode::Column { .. }
            | Mode::Repeat { .. } => {}
            Mode::Normal => {
                view.notice = None;
                let Some(pane) = view.focus() else { return };
                let Some(p) = self.panes.get_mut(&pane) else {
                    return;
                };
                let bytes = crate::encode::paste(text, p.screen().bracketed_paste());
                if let Err(error) = p.input.push(bytes)
                    && let Some(view) = self.views.get_mut(&client)
                {
                    view.error(error);
                }
            }
        }
    }

    /// The outer terminal gained or lost focus: the client's focused pane
    /// hears of it, if it asked.
    fn focus_event(&mut self, client: ClientId, gained: bool) {
        let Some(pane) = self.views.get(&client).and_then(|v| v.focus()) else {
            return;
        };
        if let Some(p) = self.panes.get_mut(&pane)
            && p.modes.focus_reporting
        {
            let _ = p.input.push(if gained {
                b"\x1b[I".to_vec()
            } else {
                b"\x1b[O".to_vec()
            });
        }
    }
}
