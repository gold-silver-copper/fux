//! What an attached client sends: typed actions (`crate::act`), routed to
//! its overlay, its copy mode, or its focused pane. The client reads its
//! own keyboard and runs its own bindings; a key reaches the server only
//! as literal input for the pane it focuses.
use crate::act::Act;
use crate::id::ClientId;
use crate::keys::Keystroke;
use crate::keys::mouse::{MouseAction, MouseEvent};
use crate::layout::Placement;
use crate::overlay;
use crate::session::Session;
use crate::view::{Mode, Notice};

impl Session {
    /// Actions from a client.
    pub fn acts(&mut self, client: ClientId, acts: Vec<Act>) {
        if acts.is_empty() {
            return;
        }
        let changes = self.changes();
        // Whether any of it may have changed what a client shows.
        let mut shown = false;
        for act in acts {
            let Some(view) = self.views.get(&client) else {
                break;
            };
            // A client that only watches is answered by its terminal, and
            // shows notices; nothing else it sends does anything.
            if view.read_only && !matches!(act, Act::Reply(_) | Act::Notice { .. }) {
                continue;
            }
            shown |= self.act(client, act);
        }
        // Keys a pane's program reads show when its output does: nothing
        // to paint or settle until then.
        if !shown && self.changes() == changes {
            return;
        }
        // A command the input ran repainted every client already; else only
        // this client's screen changed: its mode, notice or overlay.
        if self.changes() == changes
            && let Some(view) = self.views.get_mut(&client)
        {
            view.dirty = true;
        }
        self.settle();
    }

    /// One action: whether it may have changed what the client shows.
    fn act(&mut self, client: ClientId, act: Act) -> bool {
        match act {
            Act::Key(stroke) => return self.key(client, stroke),
            Act::Paste(text) => self.paste(client, &text),
            // A focus change or a mouse report for the program shows
            // nothing until it answers.
            Act::Focus(gained) => {
                self.focus_event(client, gained);
                return false;
            }
            Act::Mouse(event) => {
                self.mouse(client, event);
                return false;
            }
            Act::Reply(reply) => self.terminal_reply(client, reply),
            Act::Column(column) => self.set_mode(client, Mode::Column(column)),
            Act::Repeat(bar) => self.set_mode(client, Mode::Repeat(bar)),
            Act::Normal => {
                if let Some(Mode::Column(_) | Mode::Repeat(_)) =
                    self.views.get(&client).map(|v| &v.mode)
                {
                    self.set_mode(client, Mode::Normal);
                }
            }
            Act::Notice { text, error } => {
                if let Some(view) = self.views.get_mut(&client) {
                    view.notice = Some(Notice { text, error });
                }
            }
            Act::List(act) => overlay::list_act(self, client, act),
            Act::Prompt(act) => overlay::prompt_act(self, client, act),
            Act::Confirm(yes) => overlay::confirm(self, client, yes),
            Act::Copy(act) => crate::copy::act(self, client, act),
        }
        if let Some(view) = self.views.get_mut(&client) {
            view.dirty = true;
        }
        true
    }

    /// A key for the focused pane. Whether it may have changed what the
    /// client shows: one typed with no notice showing shows nothing until
    /// the program answers. A key typed while the column or a repeat bar
    /// shows closes it: the client has left them.
    fn key(&mut self, client: ClientId, stroke: Keystroke) -> bool {
        let Some(view) = self.views.get_mut(&client) else {
            return false;
        };
        let shown = match view.mode {
            Mode::Normal => view.notice.take().is_some(),
            Mode::Column(_) | Mode::Repeat(_) => {
                view.mode = Mode::Normal;
                true
            }
            // A key in flight as an overlay opened: dropped.
            Mode::List(_) | Mode::Prompt(_) | Mode::Confirm(_) | Mode::Copy(_) => return false,
        };
        if shown {
            view.dirty = true;
        }
        overlay::send_key(self, client, stroke);
        shown
    }

    fn paste(&mut self, client: ClientId, text: &str) {
        let Some(view) = self.views.get_mut(&client) else {
            return;
        };
        if let Mode::Prompt(_) = view.mode {
            return overlay::prompt_paste(self, client, text);
        }
        // Pastes go to the pane only.
        if !matches!(view.mode, Mode::Normal) {
            return;
        }
        view.notice = None;
        let Some(pane) = self.focused(client) else {
            return;
        };
        self.typed(client);
        let Some(p) = self.panes.get_mut(&pane) else {
            return;
        };
        let bracketed = p.screen().mode(fux_vt::Mode::BracketedPaste);
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
        let Some(pane) = self.focused(client) else {
            return;
        };
        if let Some(p) = self.panes.get_mut(&pane)
            && p.screen().mode(fux_vt::Mode::FocusReporting)
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
        let Some(pane) = self
            .focused(client)
            .filter(|_| matches!(view.mode, Mode::Normal))
        else {
            return;
        };
        let mut placement = Placement::default();
        self.placement_into(view, &mut placement);
        let Some(rect) = placement.rect(pane) else {
            return;
        };
        let inside = rect.contains(event.row, event.col);
        let seat = self.shown_tab(client).and_then(|t| t.seat(client));
        let held = seat.is_some_and(|s| s.held);
        let pressed =
            event.action == MouseAction::Press && event.button.is_some_and(|b| !b.is_wheel());
        if !(inside || held && event.action != MouseAction::Press) {
            return;
        }
        if let Some(seat) = self.shown_tab_mut(client).and_then(|t| t.seat_mut(client)) {
            if pressed {
                seat.held = true;
            } else if event.action == MouseAction::Release {
                seat.held = false;
            }
        }
        // Kept to the pane.
        let (row, col) = rect.clamp(event.row, event.col);
        let event = MouseEvent { row, col, ..event };
        self.typed(client);
        let Some(p) = self.panes.get_mut(&pane) else {
            return;
        };
        let mut report = Vec::new();
        if p.screen().encode_mouse(event, &mut report) {
            let _ = p.input.push(&report);
        }
    }
}
