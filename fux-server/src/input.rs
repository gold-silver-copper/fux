//! What an attached client sends: typed actions (`fux_wire::act`), routed to
//! its overlay or its focused pane. The client reads its
//! own keyboard and runs its own bindings; a key reaches the server only
//! as literal input for the pane it focuses.
use crate::session::Session;
use crate::view::Mode;
use fux_vt::keys::Keystroke;
use fux_vt::keys::mouse::{MouseAction, MouseEvent};
use fux_wire::act::{Act, ScreenAct};
use fux_wire::id::ClientId;
use std::time::Instant;

impl Session {
    /// Actions from a client.
    pub fn acts(&mut self, client: ClientId, acts: Vec<Act>, now: Instant) {
        if acts.is_empty() {
            return;
        }
        let previous = self.parts.actor(fux_wire::events::Actor::Client(client));
        self.client_acts(client, acts, now);
        self.parts.actor(previous);
    }

    fn client_acts(&mut self, client: ClientId, acts: Vec<Act>, now: Instant) {
        let changes = self.changes();
        let typist = |s: &Self| s.shown_tab(client).and_then(|t| t.typist);
        let typed = typist(self);
        // Whether any of it may have changed what a client shows.
        let mut shown = false;
        // A client that only watches sends no other acts than its
        // terminal's replies, notices and presentation: its own keyboard
        // holds the rest (`Keyboard`), as `read_only` is what it said itself.
        for act in acts {
            if !self.views.contains_key(&client) {
                break;
            }
            shown |= self.act(client, act);
        }
        // What a pane lost is told (`tell_lost`), each kind once a run.
        // Keys a pane's program reads show when its output does: nothing
        // to paint or settle until then.
        shown |= self.tell_lost();
        // Another client typing hosts the tab's panes, which the settle
        // tells them, painting the tab again where that changed what they
        // are told (`Session::host_panes`).
        let answers = typist(self) != typed;
        if !shown && !answers && self.changes() == changes {
            return;
        }
        // A command the input ran repainted every client already; else only
        // this client's screen changed: its mode, notice or overlay.
        if shown
            && self.changes() == changes
            && let Some(view) = self.views.get_mut(&client)
        {
            view.dirty = true;
        }
        self.settle(now);
    }

    /// One action: whether it may have changed what the client shows.
    fn act(&mut self, client: ClientId, act: Act) -> bool {
        match act {
            Act::Presentation(presentation) => {
                let bell = presentation.bell;
                if let Some(view) = self.views.get_mut(&client)
                    && view.presentation != presentation
                {
                    view.presentation = presentation;
                }
                if !bell {
                    self.parts.clear_bells(client);
                }
            }
            Act::Key(stroke) => return self.key(client, stroke),
            Act::Paste(text) => self.paste(client, &text),
            // A focus change or a mouse report for the program shows
            // nothing until it answers, but its refusal.
            Act::Focus(gained) => return self.focus_event(client, gained),
            Act::Mouse(event) => return self.mouse(client, event),
            Act::Reply(reply) => self.terminal_reply(client, reply),
            // The client's screen, drawn as it says. A notice from before
            // the screen opened or closed is old news; one the screen's
            // keys bring comes after.
            Act::Screen { opening, act } => {
                let Some(view) = self.views.get_mut(&client) else {
                    return false;
                };
                let starts = matches!(&act, ScreenAct::Show(_));
                let fresh = opening > view.opening;
                if !(fresh && starts || opening == view.opening && !view.mode.is_normal()) {
                    return false;
                }
                if fresh {
                    view.opening = opening;
                    view.mode = Mode::Normal;
                    view.notice = None;
                }
                let changed = fresh || matches!(act, ScreenAct::Hide);
                match act {
                    ScreenAct::Show(panel) => view.mode = Mode::Panel(panel),
                    ScreenAct::Hide => {
                        view.mode = Mode::Normal;
                        view.notice = None;
                    }
                }
                if changed || view.mode.is_normal() {
                    self.view_event(client, crate::journal::ViewChange::Screen);
                }
            }
            Act::Notice { text, error } => self.notify_to(client, text, error),
        }
        if let Some(view) = self.views.get_mut(&client) {
            view.dirty = true;
        }
        true
    }

    /// A key for the focused pane. Whether it may have changed what the
    /// client shows: one typed with no notice showing shows nothing until
    /// the program answers.
    fn key(&mut self, client: ClientId, stroke: Keystroke) -> bool {
        let Some(view) = self.views.get_mut(&client) else {
            return false;
        };
        // The client sends a key for its pane once it shows none of its
        // own screens: one still drawn is gone.
        let ended = !view.mode.is_normal();
        let shown = match view.mode {
            Mode::Normal => view.notice.take().is_some(),
            Mode::Panel(_) => {
                view.mode = Mode::Normal;
                true
            }
        };
        view.dirty |= shown;
        if ended {
            self.view_event(client, crate::journal::ViewChange::Screen);
        }
        // Encoded as its program asked.
        let refused = self.type_in(client, |screen, out| {
            fux_vt::keys::encode::key_bytes(stroke, screen.key_mode(), out)
        });
        shown || refused
    }

    /// Types into the client's focused pane what `write` makes for its
    /// screen: a key or a paste, as its program asked (`put_in`).
    fn type_in(
        &mut self,
        client: ClientId,
        write: impl FnOnce(&fux_vt::Screen, &mut Vec<u8>),
    ) -> bool {
        let Some(pane) = self.focused(client) else {
            return false;
        };
        self.typed(client);
        self.put_in(client, pane, write)
    }

    /// Queues for pane `pane`'s program what `write` makes for its screen,
    /// as `send-keys` does (`live_pane_mut`). Each piece refused, its
    /// program not running or its input queue refusing (`InputQueue`), is
    /// told to `client`, which sent it, the first of a run too; whether
    /// one was.
    fn put_in(
        &mut self,
        client: ClientId,
        pane: fux_wire::id::PaneId,
        write: impl FnOnce(&fux_vt::Screen, &mut Vec<u8>),
    ) -> bool {
        // Its own pane would hand it back what it typed, again and again.
        if self
            .views
            .get(&client)
            .is_some_and(|v| v.runs_in == Some(pane))
        {
            let why = format!("{pane} runs this client: what it types there is not sent");
            self.notify_to(client, why, true);
            return true;
        }
        let why = match self.live_pane_mut(pane).map(|mut p| p.push_with(write)) {
            Ok(()) => return false,
            Err(refused) => refused.to_string(),
        };
        self.notify_to(client, why, true);
        true
    }

    fn paste(&mut self, client: ClientId, text: &str) {
        let Some(view) = self.views.get_mut(&client) else {
            return;
        };
        // Pastes go to the pane only.
        if !view.mode.is_normal() {
            return;
        }
        view.notice = None;
        let _ = self.type_in(client, |screen, out| {
            fux_vt::keys::encode::paste(text, screen.mode(fux_vt::Mode::BracketedPaste), out)
        });
    }

    /// The outer terminal gained or lost focus: the client's focused pane
    /// hears of it, if it asked and its program takes input (`put_in`): a
    /// pane whose program ended is not told, nor its viewer refused.
    fn focus_event(&mut self, client: ClientId, gained: bool) -> bool {
        let Some(pane) = self.focused(client) else {
            return false;
        };
        let asked = self.parts.panes().get(&pane);
        let asked = asked.filter(|p| crate::session::refuses_input(p).is_none());
        let report = asked.and_then(|p| p.screen().focus_report(gained));
        report.is_some_and(|r| self.put_in(client, pane, |_, out| out.extend_from_slice(r)))
    }

    /// A mouse report from the client's terminal, which reports only while
    /// its focused pane's program asked for it (`render::mouse_level`):
    /// moved to the pane's cells and encoded as the program asked. fux has
    /// no mouse actions of its own: a report outside the focused pane is
    /// dropped, except that the motion and release of a press made inside
    /// are kept to the pane's edge, so the program never sees a button left
    /// down. One that arrives as an overlay opens is dropped. One the
    /// pane refuses is told (`put_in`); whether it was.
    fn mouse(&mut self, client: ClientId, event: MouseEvent) -> bool {
        let Some(view) = self.views.get(&client) else {
            return false;
        };
        let Some(pane) = self.focused(client).filter(|_| view.mode.is_normal()) else {
            return false;
        };
        let placement = self.placement(view);
        let Some(rect) = placement.rect(pane) else {
            return false;
        };
        // What feedback covers takes no presses; a release still reaches
        // its pane, kept to the pane.
        let cover = crate::render::bar_cover(self, view, &placement);
        let inside = rect.contains(event.row, event.col)
            && !cover.is_some_and(|c| c.contains(event.row, event.col));
        let seat = self.shown_tab(client).and_then(|t| t.seat(client));
        let held = seat.is_some_and(|s| s.held);
        // A press of a button, not the wheel, holds it; a release lets go.
        let (press, holds) = match event.action {
            MouseAction::Press(button) => (true, (!button.is_wheel()).then_some(true)),
            MouseAction::Release(_) => (false, Some(false)),
            MouseAction::Motion(_) => (false, None),
        };
        if !(inside || held && !press) {
            return false;
        }
        if let Some(seat) = self.shown_tab_mut(client).and_then(|t| t.seat_mut(client))
            && let Some(holds) = holds
        {
            seat.held = holds;
        }
        // Kept to the pane.
        let (row, col) = rect.clamp(event.row, event.col);
        let event = MouseEvent { row, col, ..event };
        self.typed(client);
        let Some(pane_screen) = self.parts.panes().get(&pane) else {
            return false;
        };
        let Some(reports) = pane_screen
            .screen()
            .encode_mouse(event, pane_screen.options().alternate_scroll_lines)
        else {
            return false;
        };
        self.put_in(client, pane, |_, out| out.extend(reports.flatten()))
    }
}

/// The client and the server together: bytes typed at a client's terminal
/// go through its screens and keymap (`Session::input`), as its client
/// takes them, and what it sends is what the server applies.
#[cfg(test)]
mod tests {
    use crate::session::Session;
    use crate::session::testing::{output, output_as, run};
    use crate::view::Mode;
    use fux::screens::Screen;
    use fux_wire::act::{Act, Panel, ScreenAct};
    use fux_wire::id::{ClientId, PaneId};

    type Outcome = Result<(), String>;

    /// A session without processes, one client attached.
    fn session() -> Result<(Session, ClientId), String> {
        crate::session::testing::attached(30, 100)
    }

    /// What the server draws for the client.
    fn mode(s: &Session, c: ClientId) -> String {
        match s.views.get(&c).map(|v| &v.mode) {
            Some(Mode::Normal) => "normal".into(),
            // Its keys after the prefix: `C-b g` is the layer `g`.
            Some(Mode::Panel(Panel::Column(column))) => match column.keys.split_once(' ') {
                None => format!("column {}", column.selected),
                Some((_, layer)) => format!("column {layer} {}", column.selected),
            },
            Some(Mode::Panel(Panel::Repeat(bar))) => format!("repeat {bar}"),
            Some(Mode::Panel(Panel::List(list))) => {
                format!("list {} {}", list.title, list.selected)
            }
            Some(Mode::Panel(Panel::Prompt { before, after, .. })) => {
                format!("prompt {before}|{after}")
            }
            None => "gone".into(),
        }
    }

    /// The screen the client has open.
    fn screen(s: &Session, c: ClientId) -> Screen {
        s.test_keys
            .get(&c)
            .map(|k| k.screen.screen.clone())
            .unwrap_or_default()
    }

    fn notice(s: &Session, c: ClientId) -> String {
        (s.views.get(&c).and_then(|v| v.notice.clone()))
            .map(|n| n.text)
            .unwrap_or_default()
    }

    /// Everything the client's screen shows, row by row.
    fn screen_text(s: &Session, c: ClientId) -> Result<String, String> {
        let grid = crate::render::compose(s, c).ok_or("a screen")?;
        Ok((0..grid.rows)
            .map(|y| grid.row_text(y))
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// The input waiting for a pane, taken.
    fn queued(s: &mut Session, pane: u32) -> Vec<u8> {
        (s.parts.pane_mut(PaneId::of(pane)))
            .map(|mut p| p.drain_input())
            .unwrap_or_default()
    }

    /// The input waiting for the client's focused pane, taken.
    fn queued_focused(s: &mut Session, c: ClientId) -> Vec<u8> {
        let pane = s.focused(c).map_or(0, PaneId::number);
        queued(s, pane)
    }

    fn pane_width(s: &Session, pane: u32) -> u16 {
        s.parts
            .panes()
            .get(&PaneId::of(pane))
            .map_or(0, |p| p.screen().size().cols())
    }

    /// How many tabs the first workspace has.
    fn tabs(s: &Session) -> Option<usize> {
        s.parts.workspaces().first().map(|w| w.tabs().len())
    }

    /// The prefix, then `keys`.
    fn prefixed(keys: &str) -> Vec<u8> {
        std::iter::once(0x02).chain(keys.bytes()).collect()
    }

    /// A lone Escape, taken once its wait passed.
    fn escape(s: &mut Session, c: ClientId) {
        s.input(c, b"\x1b");
        s.escape(c);
    }

    fn downs(n: usize) -> Vec<u8> {
        std::iter::repeat_n(&b"\x1b[B"[..], n)
            .flatten()
            .copied()
            .collect()
    }

    /// A tab in the first workspace.
    const NEW_TAB: &str = "opts 80 24 size str +1 workspace in new-tab";

    /// What a client types into the pane it runs in (`View::runs_in`),
    /// a key or a paste, is refused with a notice and queued nowhere; into
    /// any other pane, it goes.
    #[test]
    fn input_to_the_clients_own_pane_is_refused_and_queued_nowhere() -> Outcome {
        let (mut s, c) = session()?;
        let own = s.focused(c).ok_or("a focused pane")?;
        let _ = queued_focused(&mut s, c);
        let x: fux_vt::keys::KeyPress = "x"
            .parse()
            .map_err(|e: fux_vt::keys::Error| e.to_string())?;
        for runs_in in [Some(own), None] {
            s.views.get_mut(&c).ok_or("view")?.runs_in = runs_in;
            s.apply(c, vec![Act::Key(x.into()), Act::Paste("y".into())]);
            let typed = queued_focused(&mut s, c);
            match runs_in {
                Some(_) => {
                    assert!(typed.is_empty(), "{typed:?}");
                    assert!(
                        notice(&s, c).contains("runs this client"),
                        "{}",
                        notice(&s, c)
                    );
                }
                None => assert!(
                    typed.starts_with(b"x") && typed.ends_with(b"y"),
                    "{typed:?}"
                ),
            }
        }
        Ok(())
    }

    #[test]
    fn openings_replacements_and_closes_invalidate_from_the_prior_cursor() -> Outcome {
        use crate::journal::Reader;
        use fux_wire::events::Actor;
        use fux_wire::inspect::Scope;
        let (mut s, c) = session()?;
        s.out(PaneId::of(1), b"first\r\nlast");
        for (opening, act, normal) in [
            (1, ScreenAct::Show(Panel::Repeat("first".into())), false),
            (
                2,
                ScreenAct::Show(Panel::Repeat("replacement".into())),
                false,
            ),
            (2, ScreenAct::Hide, true),
        ] {
            let from = s.parts.journal().end(s.run);
            s.apply(c, vec![Act::Screen { opening, act }]);
            let batch = Reader::new(Scope::Client(c), s.parts.journal(), s.run, Some(from))?
                .batch(s.parts.journal(), true)?;
            assert_eq!(batch.events.len(), 1);
            let event = batch.events.first().ok_or("screen event")?;
            assert_eq!(
                event.data.get("event"),
                Some(&fux_wire::json::Json::str("screen"))
            );
            assert_eq!(event.origin, Actor::Client(c));
            let view = s.views.get(&c).ok_or("view")?;
            assert_eq!(view.opening, opening);
            assert_eq!(view.mode.is_normal(), normal);
        }
        {
            let start = ScreenAct::Show(Panel::Repeat("key ends this".into()));
            let opening = s.views.get(&c).ok_or("view")?.opening + 1;
            s.apply(
                c,
                vec![Act::Screen {
                    opening,
                    act: start,
                }],
            );
            let from = s.parts.journal().end(s.run);
            let key: fux_vt::keys::KeyPress = "x"
                .parse()
                .map_err(|e: fux_vt::keys::Error| e.to_string())?;
            s.apply(c, vec![Act::Key(key.into())]);
            let batch = Reader::new(Scope::Client(c), s.parts.journal(), s.run, Some(from))?
                .batch(s.parts.journal(), true)?;
            assert_eq!(batch.events.len(), 1);
            assert_eq!(
                batch.events.first().ok_or("ending")?.data.get("event"),
                Some(&fux_wire::json::Json::str("screen"))
            );
            assert!(s.views.get(&c).ok_or("view")?.mode.is_normal());
        }
        Ok(())
    }

    #[test]
    fn edits_within_an_opening_paint_without_screen_events() -> Outcome {
        let (mut s, c) = session()?;
        let prompt = |before: &str| Panel::Prompt {
            title: "edit".into(),
            before: before.into(),
            after: String::new(),
            hints: vec![],
        };
        let list = |selected| {
            Panel::List(fux_wire::act::List {
                items: vec!["one".into(), "two".into()],
                selected,
                ..Default::default()
            })
        };
        for (opening, before, after) in [(1, prompt("a"), prompt("ab")), (2, list(0), list(1))] {
            s.apply(
                c,
                vec![Act::Screen {
                    opening,
                    act: ScreenAct::Show(before),
                }],
            );
            let from = s.parts.journal().end(s.run);
            s.views.get_mut(&c).ok_or("view")?.dirty = false;
            s.apply(
                c,
                vec![Act::Screen {
                    opening,
                    act: ScreenAct::Show(after.clone()),
                }],
            );
            let view = s.views.get(&c).ok_or("view")?;
            assert!(matches!(&view.mode, Mode::Panel(panel) if *panel == after));
            assert!(view.dirty);
            assert_eq!(view.opening, opening);
            assert_eq!(s.parts.journal().end(s.run), from);
        }
        Ok(())
    }

    #[test]
    fn a_presentation_edit_that_resizes_a_pane_still_journals_the_resize() -> Outcome {
        use crate::journal::Reader;
        use fux_wire::inspect::Scope;
        let (mut s, c) = session()?;
        let mut presentation = s.views.get(&c).ok_or("view")?.presentation.clone();
        presentation.bar = fux_wire::presentation::Bar::new(
            fux_wire::presentation::BarPosition::Bottom,
            vec![],
            vec![],
            false,
        )?;
        let from = s.parts.journal().end(s.run);
        let rows = s
            .parts
            .panes()
            .get(&PaneId::of(1))
            .ok_or("pane")?
            .screen()
            .size()
            .rows();
        s.apply(c, vec![Act::Presentation(presentation)]);
        assert_eq!(
            s.parts
                .panes()
                .get(&PaneId::of(1))
                .ok_or("pane")?
                .screen()
                .size()
                .rows(),
            rows - 1
        );
        let batch = Reader::new(Scope::All, s.parts.journal(), s.run, Some(from))?
            .batch(s.parts.journal(), true)?;
        assert!(
            batch
                .events
                .iter()
                .any(|e| e.data.get("event") == Some(&fux_wire::json::Json::str("resized")))
        );
        Ok(())
    }

    /// Every server-side screen action belongs to one opening, including
    /// edits already in flight when that opening ended.
    #[test]
    fn stale_screen_actions_cannot_change_a_new_opening() -> Outcome {
        use fux_wire::act::{Act, ScreenAct};
        let (mut s, c) = session()?;
        s.out(PaneId::of(1), b"first\r\nlast");
        let panel = Panel::Prompt {
            title: "prompt".into(),
            before: "unsent work".into(),
            after: String::new(),
            hints: vec![],
        };
        {
            let start = ScreenAct::Show(panel.clone());
            let old = s.views.get(&c).ok_or("view")?.opening.saturating_add(1);
            let new = old.saturating_add(1);
            s.apply(
                c,
                vec![
                    Act::Screen {
                        opening: old,
                        act: start.clone(),
                    },
                    Act::Screen {
                        opening: new,
                        act: start.clone(),
                    },
                ],
            );
            let before = mode(&s, c);
            for act in [
                ScreenAct::Hide,
                ScreenAct::Show(Panel::Repeat("old panel".into())),
                start.clone(),
            ] {
                s.apply(c, vec![Act::Screen { opening: old, act }]);
                assert_eq!(mode(&s, c), before);
                assert_eq!(s.views.get(&c).ok_or("view")?.opening, new);
            }
            s.apply(
                c,
                vec![Act::Screen {
                    opening: new,
                    act: ScreenAct::Hide,
                }],
            );
            assert_eq!(mode(&s, c), "normal");
            s.apply(
                c,
                vec![Act::Screen {
                    opening: new,
                    act: start,
                }],
            );
            assert_eq!(mode(&s, c), "normal", "ended openings cannot redraw");
        }
        Ok(())
    }

    /// A replacement key panel starts a new opening; navigation within
    /// its column keeps the opening and entering repeat clears old errors.
    #[test]
    fn replacing_a_key_panel_ends_its_old_notice() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "%1 str horizontal axis split drop")?;
        s.input(c, b"\x02");
        let prefix = s.views.get(&c).ok_or("view")?.opening;
        s.input(c, b"r");
        let column = s.views.get(&c).ok_or("view")?.opening;
        s.input(c, b"\x1b[B");
        assert_eq!(s.views.get(&c).ok_or("view")?.opening, column);
        s.input(c, b"?");
        assert!(notice(&s, c).contains("is not bound"));
        let width = pane_width(&s, 2);
        s.input(c, b"h");
        let view = s.views.get(&c).ok_or("view")?;
        assert!(matches!(view.mode, Mode::Panel(Panel::Repeat(_))));
        assert_ne!(view.opening, column, "repeat is a new opening");
        assert_ne!(column, prefix, "a different column is a new opening");
        assert!(view.notice.is_none(), "{}", notice(&s, c));
        let repeat = view.opening;
        assert_ne!(pane_width(&s, 2), width, "the resize succeeded");
        s.input(c, b"h");
        assert_eq!(s.views.get(&c).ok_or("view")?.opening, repeat);
        s.input(c, b"\x02");
        assert!(s.views.get(&c).ok_or("view")?.opening > repeat);
        Ok(())
    }

    /// A name typed into a rename prompt is the name, whatever it looks
    /// like.
    #[test]
    fn a_rename_prompt_takes_any_name() -> Outcome {
        let (mut s, c) = session()?;
        for (open, name, read) in [
            ("str c1 client str %1 pane rename-prompt", "-dev", "%1"),
            ("str c1 client str @1 tab rename-prompt", "--", "@1"),
            (
                "str c1 client str +1 workspace rename-prompt",
                "x 'w' --",
                "+1",
            ),
        ] {
            run(&mut s, open)?;
            // Backspace clears what the prompt starts with, the current name.
            s.input(c, &[0x7f; 16]);
            s.input(c, name.as_bytes());
            s.input(c, b"\r");
            assert_eq!(notice(&s, c), "", "{open}");
            assert_eq!(mode(&s, c), "normal", "{open}");
            let listed = output(&mut s, "ls listing")?;
            assert!(
                listed.contains(&format!("{read} {name}")),
                "{open}: {listed}"
            );
        }
        Ok(())
    }

    /// A screen's keys are the config's, as the bindings are: one rebound
    /// acts, and its hint says so; Escape always leaves, and is never bound.
    #[test]
    fn a_screens_keys_are_the_configs() -> Outcome {
        let (mut s, c) = session()?;
        let config = "str list str q unbind-in  str list str n unbind-in  \
                      str list str g str cancel bind-in  str done label";
        output_as(&mut s, Some(c), config)?;
        for (bad, said) in [
            ("str list str Escape str first bind-in", "cannot be bound"),
            ("str list str g str nowhere bind-in", "\"nowhere\" is not "),
        ] {
            let refused = output_as(&mut s, Some(c), bad).err().unwrap_or_default();
            assert!(refused.contains(said), "{refused}");
        }
        s.input(c, b"\x02tg");
        let hints = match s.views.get(&c).map(|v| &v.mode) {
            Some(Mode::Panel(Panel::List(list))) => {
                list.hints.iter().map(|h| h.keys.clone()).collect()
            }
            _ => Vec::new(),
        };
        assert!(hints.contains(&"g Escape".to_owned()), "{hints:?}");
        s.input(c, b"q");
        assert!(mode(&s, c).starts_with("list "), "{}", mode(&s, c));
        s.input(c, b"g");
        assert_eq!(mode(&s, c), "normal");
        s.input(c, b"\x02tg");
        escape(&mut s, c);
        assert_eq!(mode(&s, c), "normal");
        Ok(())
    }

    /// The confirmation's recipe file, loaded.
    const CONFIRM: &str = concat!(
        "require ",
        env!("CARGO_MANIFEST_DIR"),
        "/../recipes/confirm.fux"
    );

    /// A question's or a prompt's action is checked as it opens: one with
    /// a mistake opens nothing.
    #[test]
    fn an_action_is_checked_as_its_screen_opens() -> Outcome {
        let (mut s, c) = session()?;
        for (action, said) in [
            ("str drop".to_owned(), "code … end"),
            ("code nosuch end".to_owned(), "no such word"),
            (
                "list empty code drop end code drop end end".to_owned(),
                "a callback is",
            ),
        ] {
            let open = format!("{CONFIRM} me str sure? {action} confirm");
            let refused = output_as(&mut s, Some(c), &open).err().unwrap_or_default();
            assert!(refused.contains(said), "{refused}");
            assert_eq!(mode(&s, c), "normal");
        }
        // A prompt's start is a line as typing makes one: no line end.
        let open = "me str t list str a 10 char str b end glue code drop end prompt";
        let refused = output_as(&mut s, Some(c), open).err().unwrap_or_default();
        assert!(refused.contains("bad_value"), "{refused}");
        assert_eq!(mode(&s, c), "normal");
        Ok(())
    }

    /// A confirmation's one row, Yes, chosen by `y` or Enter, runs its
    /// callback on the pane it captured; `n` and Escape cancel, and the
    /// list's row actions, `r` and `x`, refuse its row: it is no pane, tab
    /// or workspace.
    #[test]
    fn a_confirmation_runs_its_callback_by_y_or_enter_alone() -> Outcome {
        let (mut s, c) = session()?;
        let ask =
            &format!("{CONFIRM} me str sure? list list %1 end code str yes rename end end confirm");
        for key in ["n", "Escape", "r", "x"] {
            output_as(&mut s, Some(c), ask)?;
            assert_eq!(mode(&s, c), "list sure? 0");
            match key {
                "Escape" => escape(&mut s, c),
                key => s.input(c, key.as_bytes()),
            }
            assert_eq!(mode(&s, c), "normal", "{key}: {}", notice(&s, c));
            if ["r", "x"].contains(&key) {
                let said = notice(&s, c);
                assert!(said.contains("not a boolean"), "{key}: {said}");
            }
            assert_eq!(output(&mut s, "%1 str name get")?, "null\n", "{key}");
        }
        for key in ["y", "\r"] {
            run(&mut s, "%1 str no rename")?;
            output_as(&mut s, Some(c), ask)?;
            s.input(c, key.as_bytes());
            assert_eq!(output(&mut s, "%1 str name get")?, "yes\n", "{key:?}");
        }
        Ok(())
    }

    /// On a screen too short for a list's lines, the selected entry stays
    /// in view: the lines around it give way first.
    #[test]
    fn a_short_screen_keeps_the_selected_entry_in_view() -> Outcome {
        let (mut s, c) = crate::session::testing::attached(3, 40)?;
        for _ in 0..3 {
            run(&mut s, NEW_TAB)?;
        }
        run(&mut s, "str c1 client dup focused tab choose-among")?;
        s.input(c, b"\x1b[H");
        let shown = screen_text(&s, c)?;
        assert!(shown.contains("@1 %1"), "{shown}");
        Ok(())
    }

    /// Unbound upper-case keys do not act as their lower-case bindings.
    #[test]
    fn list_and_confirm_keys_match_exact_case() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, NEW_TAB)?;
        run(&mut s, "str c1 client dup focused tab choose-among")?;
        s.input(c, b"\x1b[HJ");
        assert!(mode(&s, c).ends_with(" 0"));
        s.input(c, b"j");
        assert!(mode(&s, c).ends_with(" 1"));
        s.input(c, b"q");
        run(&mut s, "str c1 client str @2 tab confirm-close")?;
        s.input(c, b"Y");
        assert_eq!(tabs(&s), Some(2));
        s.input(c, b"y");
        assert_eq!(tabs(&s), Some(1));
        Ok(())
    }

    /// A long chooser scrolled to the middle shows its title, both "more"
    /// lines, its items and its help, all within the screen (41 tabs, the
    /// cursor at the 31st, on 30 rows lost the title).
    #[test]
    fn a_scrolled_list_keeps_its_title() -> Outcome {
        let (mut s, c) = session()?;
        for _ in 0..40 {
            run(&mut s, NEW_TAB)?;
        }
        run(&mut s, "str c1 client dup focused tab choose-among")?;
        s.input(c, b"\x1b[H");
        s.input(c, &downs(30));
        let Some(Mode::Panel(Panel::List(list))) = s.views.get(&c).map(|v| &v.mode) else {
            return Err(mode(&s, c));
        };
        assert_eq!(list.selected, 30);
        let title = list.title.clone();
        let text = screen_text(&s, c)?;
        for shown in [title.as_str(), "▲", "▼", "Enter y select"] {
            assert!(text.contains(shown), "{shown:?} in\n{text}");
        }
        Ok(())
    }

    /// A layer and a repeat mode, bound here rather than by default.
    fn with_layers(s: &mut Session) -> Outcome {
        run(
            s,
            "str 'g n' code opts str / cwd new-tab drop end bind str 'new tab' label str Tabs group",
        )?;
        run(
            s,
            "str 'y l' code me focused str right 1 move-border end bind str Grow group repeat",
        )?;
        run(
            s,
            "str 'y h' code me focused str left 1 move-border end bind str Grow group repeat",
        )
    }

    /// A layer's key opens it, its column titled and showing the keys so
    /// far; a key it does not bind keeps it open, saying so; its binding
    /// runs on the next key; the prefix inside it goes to the pane; and
    /// Enter on its entry in the column opens it too.
    #[test]
    fn a_layer_runs_its_binding_on_the_next_key() -> Outcome {
        let (mut s, c) = session()?;
        with_layers(&mut s)?;
        s.input(c, b"\x02g");
        assert_eq!(mode(&s, c), "column g 0");
        let text = screen_text(&s, c)?;
        assert!(text.contains("C-b g: Tabs"), "{text}");
        assert!(text.contains("new tab"), "{text}");
        s.input(c, b"f");
        assert_eq!(mode(&s, c), "column g 0");
        assert_eq!(notice(&s, c), "C-b g f is not bound");
        s.input(c, b"n");
        assert_eq!(mode(&s, c), "normal");
        assert_eq!(tabs(&s), Some(2));
        let _ = queued_focused(&mut s, c);
        s.input(c, b"\x02g\x02");
        assert_eq!(mode(&s, c), "normal");
        assert_eq!(queued_focused(&mut s, c), b"\x02");
        let at = (s.test_keymap.keys.column(&[], 0).ok_or("a column")?.entries)
            .iter()
            .position(|e| e.key.to_string() == "g")
            .ok_or("the layer is listed")?;
        s.input(c, b"\x02");
        s.input(c, &downs(at));
        s.input(c, b"\r");
        assert_eq!(mode(&s, c), "column g 0");
        Ok(())
    }

    #[test]
    fn a_repeating_binding_keeps_its_layer_until_enter_or_esc() -> Outcome {
        let (mut s, c) = session()?;
        with_layers(&mut s)?;
        run(&mut s, "str %1 pane str horizontal axis split")?;
        run(&mut s, "str c1 client str %1 pane show")?;
        let start = pane_width(&s, 1);
        s.input(c, b"\x02y");
        assert_eq!(mode(&s, c), "column y 0");
        s.input(c, b"l");
        // The bar names the mode and its keys.
        assert_eq!(mode(&s, c), "repeat GROW  l h · Esc");
        let grid = crate::render::compose(&s, c).ok_or("a screen")?;
        let bar = grid.row_text(grid.rows.saturating_sub(1));
        assert!(bar.contains("GROW  l h · Esc"), "{bar}");
        // No prefix: the mode's keys run again and again.
        s.input(c, b"ll");
        assert_eq!(start.checked_add(3), Some(pane_width(&s, 1)));
        s.input(c, b"h");
        assert_eq!(start.checked_add(2), Some(pane_width(&s, 1)));
        s.input(c, b"\r");
        assert_eq!(mode(&s, c), "normal");
        // Esc leaves too, once its wait passes.
        s.input(c, b"\x02yl");
        escape(&mut s, c);
        assert_eq!(mode(&s, c), "normal");
        assert_eq!(start.checked_add(3), Some(pane_width(&s, 1)));
        // After it, `l` is the pane's again.
        let _ = queued(&mut s, 1);
        s.input(c, b"l");
        assert_eq!(queued(&mut s, 1), b"l");
        Ok(())
    }

    #[test]
    fn another_key_leaves_a_repeat_mode_without_reaching_the_pane() -> Outcome {
        let (mut s, c) = session()?;
        with_layers(&mut s)?;
        s.input(c, b"\x02yl");
        let _ = queued(&mut s, 1);
        s.input(c, b"f");
        assert_eq!(mode(&s, c), "normal");
        assert_eq!(notice(&s, c), "f is not one of this mode's keys");
        assert!(queued(&mut s, 1).is_empty());
        // The prefix leaves it for the column.
        s.input(c, b"\x02yl\x02");
        assert_eq!(mode(&s, c), "column 0");
        Ok(())
    }

    #[test]
    fn a_repeat_mode_may_bind_enter() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str %1 pane str horizontal axis split")?;
        run(&mut s, "str c1 client str %1 pane show")?;
        let start = pane_width(&s, 1);
        run(
            &mut s,
            "str 'r Enter' code me focused str right 1 move-border end bind repeat",
        )?;
        s.input(c, &prefixed("r\r\r"));
        assert!(mode(&s, c).starts_with("repeat "), "{}", mode(&s, c));
        assert_eq!(start.checked_add(2), Some(pane_width(&s, 1)));
        escape(&mut s, c);
        assert_eq!(mode(&s, c), "normal");
        Ok(())
    }

    /// The panes of the client's tab, left to right.
    fn pane_order(s: &mut Session) -> Result<Vec<String>, String> {
        let ls = output(s, "ls listing")?;
        Ok(ls
            .lines()
            .filter_map(|l| l.trim_start().split(' ').next())
            .filter(|w| w.starts_with('%'))
            .map(str::to_owned)
            .collect())
    }

    #[test]
    fn move_mode_moves_a_pane_until_esc() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str %1 pane str horizontal axis split")?;
        run(&mut s, "str %2 pane str horizontal axis split")?;
        run(&mut s, "str c1 client str %3 pane show")?;
        assert_eq!(pane_order(&mut s)?, ["%1", "%2", "%3"]);
        s.input(c, &prefixed("mhh"));
        escape(&mut s, c);
        assert_eq!(mode(&s, c), "normal");
        assert_eq!(pane_order(&mut s)?, ["%3", "%1", "%2"]);
        Ok(())
    }

    #[test]
    fn the_tab_layer_reorders_in_a_repeat_mode() -> Outcome {
        let (mut s, c) = session()?;
        run(
            &mut s,
            "opts 80 24 size str +1 workspace in str two name new-tab",
        )?;
        run(
            &mut s,
            "opts 80 24 size str +1 workspace in str three name new-tab",
        )?;
        run(&mut s, "str c1 client str @1 tab show")?;
        s.input(c, &prefixed("tmll"));
        assert_eq!(mode(&s, c), "repeat REORDER  h l · Esc");
        escape(&mut s, c);
        let order: Vec<String> = (s.parts.workspaces().first())
            .map(|w| {
                w.tabs()
                    .iter()
                    .map(|t| crate::workspace::Child::id(t).to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(order, ["@2", "@3", "@1"]);
        Ok(())
    }

    #[test]
    fn keys_after_the_prefix_match_as_typed() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str V code opts str / cwd new-tab drop end bind")?;
        s.input(c, &prefixed("V"));
        assert_eq!(tabs(&s), Some(2));
        assert_eq!(mode(&s, c), "normal");
        // An upper-case letter bound to nothing (Caps Lock on) says so, and
        // is not taken for its lower case.
        s.input(c, &prefixed("T"));
        assert_eq!(notice(&s, c), "C-b T is not bound");
        assert_eq!(tabs(&s), Some(2));
        escape(&mut s, c);
        // A letter with Ctrl is no binding's.
        s.input(c, &prefixed("\x14"));
        assert_eq!(notice(&s, c), "C-b C-t is not bound");
        Ok(())
    }

    #[test]
    fn a_binding_wins_over_the_columns_own_keys() -> Outcome {
        let (mut s, c) = session()?;
        // Unbound, Up and Down move the column's selection.
        s.input(c, &prefixed("\x1b[B\x1b[B\x1b[A"));
        assert_eq!(mode(&s, c), "column 1");
        escape(&mut s, c);
        run(&mut s, "str Up code opts str / cwd new-tab drop end bind")?;
        s.input(c, &prefixed("\x1b[A"));
        assert_eq!(tabs(&s), Some(2));
        assert_eq!(mode(&s, c), "normal");
        // Down, still unbound, still moves.
        s.input(c, &prefixed("\x1b[B"));
        assert_eq!(mode(&s, c), "column 1");
        escape(&mut s, c);
        // The prefix twice sends it, until the layer binds it.
        let _ = queued_focused(&mut s, c);
        s.input(c, &[0x02, 0x02]);
        assert_eq!(queued_focused(&mut s, c), b"\x02");
        run(&mut s, "str C-b code opts str / cwd new-tab drop end bind")?;
        s.input(c, &[0x02, 0x02]);
        assert_eq!(tabs(&s), Some(3));
        assert!(queued_focused(&mut s, c).is_empty());
        // A digit, a function key and a chord bind as letters do.
        for key in ["1", "F5", "M-n"] {
            run(
                &mut s,
                &format!("str {key} code opts str / cwd new-tab drop end bind"),
            )?;
        }
        s.input(c, &prefixed("1"));
        s.input(c, &prefixed("\x1b[15~"));
        s.input(c, &prefixed("\x1bn"));
        assert_eq!(tabs(&s), Some(6));
        Ok(())
    }

    /// Keys bound without the prefix are listed under headings of their
    /// own, even one named as a group of keys typed after it is.
    #[test]
    fn keys_without_the_prefix_share_no_heading() -> Outcome {
        let (mut s, c) = crate::session::testing::attached(100, 100)?;
        run(
            &mut s,
            "str F2 code me focused zoom end bind-root str Session group",
        )?;
        s.input(c, b"\x02");
        let text = screen_text(&s, c)?;
        let headings = text.lines().filter(|l| l.trim() == "Session").count();
        assert_eq!(headings, 2, "{text}");
        Ok(())
    }

    #[test]
    fn a_key_bound_without_the_prefix_runs_at_once_and_reaches_no_pane() -> Outcome {
        let (mut s, c) = session()?;
        run(
            &mut s,
            "str M-t code opts str / cwd new-tab drop end bind-root",
        )?;
        s.input(c, b"\x1bt");
        assert_eq!(tabs(&s), Some(2));
        assert!(queued_focused(&mut s, c).is_empty());
        // A chord bound to nothing reaches the pane as it was typed.
        s.input(c, b"\x1bl");
        assert_eq!(queued_focused(&mut s, c), b"\x1bl");
        // In the column the key is the column's, not the binding's.
        s.input(c, &prefixed("\x1bt"));
        assert_eq!(tabs(&s), Some(2));
        assert_eq!(notice(&s, c), "C-b M-t is not bound");
        escape(&mut s, c);
        // The column lists it last, and runs it.
        let entries = s.test_keymap.keys.column(&[], 0).ok_or("a column")?.entries;
        assert!(entries.last().is_some_and(|e| e.root), "{entries:?}");
        s.input(c, b"\x02");
        s.input(c, &downs(entries.len().saturating_sub(1)));
        s.input(c, b"\r");
        assert_eq!(tabs(&s), Some(3));
        Ok(())
    }

    /// A session with room for every command to act: two workspaces, the
    /// first with two tabs, its first with three panes, 1 | (2 / 3), the
    /// client on %2.
    fn busy() -> Result<(Session, ClientId), String> {
        let (mut s, c) = session()?;
        run(&mut s, "str %1 pane str horizontal axis split")?;
        run(&mut s, "str %2 pane str vertical axis split")?;
        run(&mut s, NEW_TAB)?;
        run(&mut s, "opts str new-workspace in 80 24 size new-tab")?;
        run(&mut s, "str c1 client str %2 pane show")?;
        Ok((s, c))
    }

    /// What a user can tell apart: every workspace, tab, pane and client,
    /// what the server draws for the client, its notice, and the screen
    /// its client has open.
    fn state(s: &mut Session, c: ClientId) -> Result<String, String> {
        // Opening the prefix panel and leaving it are real journal events;
        // direct evaluation has a different history, with the same state.
        let ls = output(s, "ls listing")?
            .lines()
            .filter(|line| !line.starts_with("cursor "))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(format!(
            "{ls}{}\n{}\n{:?}",
            mode(s, c),
            notice(s, c),
            screen(s, c)
        ))
    }

    /// Typing a default binding's keys does what running its program as the
    /// client does, and the program runs; a repeating one also enters its
    /// mode.
    #[test]
    fn every_default_binding_runs_its_program() -> Outcome {
        let keymap = fux::script::default_state()
            .map(|state| state.keys)
            .map_err(|e| e.to_string())?;
        let bindings = keymap.bindings.all();
        assert!(!bindings.is_empty());
        let mut refused = Vec::new();
        for (path, binding, repeat) in bindings {
            let keys: String = path.iter().map(fux_vt::keys::KeyPress::to_string).collect();
            let (mut typed, c) = busy()?;
            typed.input(c, &prefixed(&keys));
            if repeat {
                assert!(
                    mode(&typed, c).starts_with("repeat "),
                    "{keys}: {}",
                    mode(&typed, c)
                );
                typed.input(c, b"\r");
            }
            let (mut ran, d) = busy()?;
            if let Err(error) = output_as(&mut ran, Some(d), &binding.code.text()) {
                refused.push(keys.clone());
                // Leaving a repeat mode clears what its keys said.
                if !repeat {
                    ran.notify_to(d, error, true);
                }
            }
            assert_eq!(state(&mut typed, c)?, state(&mut ran, d)?, "{keys}");
        }
        // Refused as the session stands: no pane that way, nothing before
        // the first tab or workspace, nothing but a shell to signal, and no
        // readable nearby cwd in these processless panes.
        refused.sort();
        assert_eq!(
            refused,
            [
                "ak", "c", "k", "l", "mk", "ml", "s", "tmh", "tn", "v", "wmh", "wn"
            ]
        );
        Ok(())
    }

    #[test]
    fn every_library_chooser_builds_its_rows_from_one_topology_observation() -> Outcome {
        use fux::script::{AsClient, Asks, Host};
        use fux_wire::command::Command;
        struct Reads<'a>(crate::session::testing::InProcess<'a>, Vec<Command>);
        impl Asks for Reads<'_> {
            fn ask(
                &mut self,
                asked: &fux_wire::protocol::Asked,
            ) -> Result<fux_wire::protocol::Reply, fux::script::Failed> {
                if let fux_wire::command::Request::Do(command) = &asked.request
                    && !matches!(command, Command::Resolve { .. })
                {
                    self.1.push(command.clone());
                }
                self.0.ask(asked)
            }
        }
        for (code, captures) in [
            ("me me focused tab choose-among", 0),
            ("me me focused workspace choose-among", 0),
            ("me %2 %2 tab choose-move", 1),
            ("me %2 %2 workspace choose-move", 1),
            ("me %2 choose-pane", 1),
        ] {
            let (mut s, c) = busy()?;
            let context = fux_wire::protocol::Context {
                client: Some(c),
                of: Some(s.run),
                ..Default::default()
            };
            let mut state = s.test_keymap.clone();
            let mut reads = Reads(crate::session::testing::InProcess(&mut s), Vec::new());
            let mut asks = AsClient {
                asks: &mut reads,
                context: context.clone(),
                opened: None,
                on_commit: None,
                stop: None,
            };
            let mut machine = fux::script::compile(
                &fuxth::token::source("chooser", code).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            let mut host = Host::new(context, Some(&mut state), &mut out);
            fux::script::run(&mut machine, &mut host, &mut asks, None)
                .map_err(|e| e.to_string())?;
            let Some(Screen::List(list)) = asks.opened else {
                return Err(format!("{code}: no list"));
            };
            assert!(!list.items.is_empty());
            assert!(
                list.items
                    .iter()
                    .all(|item| matches!(item.payload, fux_wire::callback::Data::Handle(..))),
                "{code}: a row is not a destination handle"
            );
            assert_eq!(
                list.action.args.len(),
                captures,
                "{code}: the source should be captured once"
            );
            assert!(
                matches!(
                    reads.1.as_slice(),
                    [Command::Inspect {
                        projection: fux_wire::inspect::Projection::Topology,
                        ..
                    }]
                ),
                "{code}: {:?}",
                reads.1
            );
        }
        Ok(())
    }

    #[test]
    fn library_chooser_callbacks_act_on_the_selected_destination() -> Outcome {
        for (open, keys, observe, expected) in [
            (
                "me me focused tab choose-among",
                "j\r",
                "me focused tab string",
                "@2\n",
            ),
            (
                "me me focused workspace choose-among",
                "j\r",
                "me focused workspace string",
                "+2\n",
            ),
            ("me %2 %2 tab choose-move", "j\r", "%2 tab string", "@2\n"),
            (
                "me %2 %2 workspace choose-move",
                "j\r",
                "%2 workspace string",
                "+2\n",
            ),
            (
                "me %2 choose-pane",
                "\r",
                "@1 panes each string print end",
                "%2\n%1\n%3\n",
            ),
        ] {
            let (mut s, c) = busy()?;
            output_as(&mut s, Some(c), open)?;
            s.input(c, keys.as_bytes());
            assert_eq!(notice(&s, c), "", "{open}");
            assert_eq!(screen(&s, c), Screen::default(), "{open}");
            assert_eq!(output_as(&mut s, Some(c), observe)?, expected, "{open}");
        }
        Ok(())
    }

    #[test]
    fn list_rename_and_close_callbacks_address_each_choosers_destination() -> Outcome {
        for (open, target, kind) in [
            ("me me focused tab choose-among", "@1", "tab"),
            ("me me focused workspace choose-among", "+1", "workspace"),
            ("me %2 %2 tab choose-move", "@1", "tab"),
            ("me %2 %2 workspace choose-move", "+1", "workspace"),
            ("me %2 choose-pane", "%1", "pane"),
        ] {
            let (mut s, c) = busy()?;
            output_as(&mut s, Some(c), open)?;
            s.input(c, b"r");
            let Screen::Prompt(prompt) = screen(&s, c) else {
                return Err(format!("{open}: rename did not open: {}", notice(&s, c)));
            };
            assert_eq!(prompt.title, format!("rename {kind} {target}"));
            s.input(c, &[0x7f; 16]);
            s.input(c, b"destination\r");
            assert_eq!(
                output(&mut s, &format!("{target} str name get"))?,
                "destination\n",
                "{open}"
            );
            assert_eq!(output(&mut s, "%2 str name get")?, "null\n", "{open}");
            output_as(&mut s, Some(c), open)?;
            s.input(c, b"x");
            assert_eq!(
                mode(&s, c),
                format!("list close {kind} {target} destination? 0"),
                "{open}: {}",
                notice(&s, c)
            );
            s.input(c, b"y");
            assert!(
                output(&mut s, &format!("{target} str name get")).is_err(),
                "{open}"
            );
        }
        Ok(())
    }

    #[test]
    fn list_callbacks_are_explicit_and_labels_remain_inert() -> Outcome {
        let (mut s, c) = session()?;
        // A valid executable word is still not a native list action.
        for bad in [
            "str list str R str confirm-close bind-in",
            "str list str R str rename bind-in",
            "str R str nosuch bind-action",
            "str Escape code 2drop end bind-action",
        ] {
            assert!(output_as(&mut s, Some(c), bad).is_err(), "{bad}");
        }
        output_as(
            &mut s,
            Some(c),
            "str R code nip print end bind-action str test label",
        )?;
        run(
            &mut s,
            "str c1 client str hosts list list str '%1 kill' str '%1 kill' end end list end list 0 end code nip print end choose",
        )?;
        s.input(c, b"R");
        assert!(notice(&s, c).contains("%1 kill"), "{}", notice(&s, c));
        assert!(s.parts.panes().contains_key(&PaneId::of(1)));
        Ok(())
    }

    #[test]
    fn chooser_selection_is_explicit_and_independent_of_the_current_marker() -> Outcome {
        let (mut s, c) = session()?;
        for (rows, current, selected, accepted) in [
            ("list end", "list end", "list end", true),
            ("list end", "list 0 end", "list end", false),
            (
                "list list 7 str seven end end",
                "list end",
                "list end",
                false,
            ),
            (
                "list list 7 str seven end end",
                "list end",
                "list -1 end",
                false,
            ),
            (
                "list list 7 str seven end end",
                "list end",
                "list 1 end",
                false,
            ),
        ] {
            let code = format!("me str title {rows} {current} {selected} code 2drop end choose");
            assert_eq!(
                output_as(&mut s, Some(c), &code).is_ok(),
                accepted,
                "{code}"
            );
            escape(&mut s, c);
        }
        output_as(
            &mut s,
            Some(c),
            "me str title list list 7 str seven end list 8 str eight end end list 1 end list 0 end code 2drop end choose",
        )?;
        let Screen::List(list) = screen(&s, c) else {
            return Err(mode(&s, c));
        };
        assert_eq!((list.current, list.selected), (Some(1), 0));
        Ok(())
    }

    #[test]
    fn a_stale_selected_row_never_retargets_a_new_object_with_the_same_label() -> Outcome {
        let (mut s, c) = session()?;
        run(
            &mut s,
            "@1 str shared rename opts 80 24 size str +1 workspace in new-tab drop",
        )?;
        output_as(&mut s, Some(c), "me me focused tab choose-among")?;
        run(
            &mut s,
            "@1 kill opts 80 24 size str +1 workspace in str shared name new-tab drop",
        )?;
        assert!(matches!(screen(&s, c), Screen::List(_)));
        let before = s.place(c).ok_or("place")?.tab;
        s.input(c, b"\r");
        assert_eq!(s.place(c).ok_or("place")?.tab, before);
        assert!(notice(&s, c).contains("@1"), "{}", notice(&s, c));
        Ok(())
    }

    /// Pane `pane`'s rect on client `c`'s screen.
    fn rect_of(s: &Session, c: ClientId, pane: u32) -> Option<fux_wire::layout::Rect> {
        s.placement(s.views.get(&c)?).rect(PaneId::of(pane))
    }

    /// The mouse tracking client `c`'s terminal is asked for.
    fn asked_mouse_level(s: &Session, c: ClientId) -> Option<u16> {
        crate::render::compose(s, c).map(|g| g.modes.mouse)
    }

    #[test]
    fn a_mouse_report_reaches_the_focused_pane_in_its_cells() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str %1 pane str horizontal axis split")?;
        run(&mut s, "str c1 client str %2 pane show")?;
        let right = rect_of(&s, c, 2).ok_or("no rect for %2")?;
        assert!(right.x() > 1 && right.y() == 0, "{right:?}");
        // Nothing asked: no reporting, and a report is dropped.
        assert_eq!(asked_mouse_level(&s, c), Some(0));
        let press = |col: u16, row: u16| format!("\x1b[<0;{};{}M", col + 1, row + 1);
        s.input(c, press(right.x() + 3, 2).as_bytes());
        assert!(queued(&mut s, 2).is_empty());
        s.out(PaneId::of(2), b"\x1b[?1000h\x1b[?1006h");
        assert_eq!(asked_mouse_level(&s, c), Some(1000));
        // Moved to the pane's cells.
        s.input(c, press(right.x() + 3, 2).as_bytes());
        assert_eq!(queued(&mut s, 2), b"\x1b[<0;4;3M");
        // On the other pane, or the border between them: dropped.
        s.input(c, press(1, 2).as_bytes());
        s.input(c, press(right.x() - 1, 2).as_bytes());
        assert!(queued(&mut s, 2).is_empty() && queued(&mut s, 1).is_empty());
        // In the encoding the program asked for: here the default.
        s.out(PaneId::of(2), b"\x1b[?1006l");
        s.input(c, press(right.x(), 0).as_bytes());
        assert_eq!(queued(&mut s, 2), [0x1b, b'[', b'M', 32, 33, 33]);
        // A pane below another: moved down too.
        run(&mut s, "str %2 pane str vertical axis split")?;
        run(&mut s, "str c1 client str %3 pane show")?;
        let below = rect_of(&s, c, 3).ok_or("no rect for %3")?;
        assert!(below.y() > 1, "{below:?}");
        s.out(PaneId::of(3), b"\x1b[?1000h\x1b[?1006h");
        s.input(c, press(below.x() + 1, below.y() + 2).as_bytes());
        assert_eq!(queued(&mut s, 3), b"\x1b[<0;2;3M");
        Ok(())
    }

    #[test]
    fn bar_feedback_blocks_presses_where_it_covers_but_keeps_a_held_release() -> Outcome {
        let (mut s, c) = crate::session::testing::attached(2, 20)?;
        s.size_client(c, 1, 20).map_err(|e| e.to_string())?;
        s.out(PaneId::of(1), b"\x1b[?1000h\x1b[?1006h");
        s.input(c, b"\x1b[<0;1;1M");
        assert_eq!(queued(&mut s, 1), b"\x1b[<0;1;1M");
        // " refused " covers the row's last ten cells.
        s.notify_to(c, "refused", true);
        s.input(c, b"\x1b[<0;16;1M");
        assert!(queued(&mut s, 1).is_empty());
        s.input(c, b"\x1b[<0;16;1m");
        assert_eq!(queued(&mut s, 1), b"\x1b[<0;16;1m");
        assert!(
            s.shown_tab(c)
                .and_then(|t| t.seat(c))
                .is_some_and(|seat| !seat.held)
        );
        // Left of it, the row is the pane's.
        s.input(c, b"\x1b[<0;2;1M");
        assert_eq!(queued(&mut s, 1), b"\x1b[<0;2;1M");
        Ok(())
    }

    /// A key or paste typed into a pane whose program exited is refused,
    /// as `send-keys` is, and told: never queued for no one to read.
    #[test]
    fn a_key_into_an_exited_program_is_refused_and_told() -> Outcome {
        let (mut s, c) = session()?;
        run(
            &mut s,
            "opts str agent in 80 24 size str /bin/cat program new-tab",
        )?;
        run(&mut s, "str c1 client str %2 pane show")?;
        s.end_program(PaneId::of(2), fux_wire::answer::Ended::Exited(3));
        for typed in [b"x".as_slice(), b"\x1b[200~z\x1b[201~"] {
            s.input(c, typed);
            let notice = s.views.get_mut(&c).and_then(|v| v.notice.take());
            let told = notice.map(|n| n.text).unwrap_or_default();
            assert!(told.contains("the program in %2 exited 3"), "{told}");
            assert!(queued(&mut s, 2).is_empty());
        }
        Ok(())
    }

    #[test]
    fn a_drag_out_of_the_pane_is_kept_to_its_edge() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str %1 pane str horizontal axis split")?;
        run(&mut s, "str c1 client str %2 pane show")?;
        let right = rect_of(&s, c, 2).ok_or("no rect for %2")?;
        s.out(PaneId::of(2), b"\x1b[?1002h\x1b[?1006h");
        assert_eq!(asked_mouse_level(&s, c), Some(1002));
        let x = right.x() + 1;
        // Pressed inside, dragged over the left pane and past the bottom,
        // released there: the pane hears all of it, at its edge.
        s.input(c, format!("\x1b[<0;{};2M", x + 1).as_bytes());
        s.input(c, b"\x1b[<32;1;2M");
        s.input(c, b"\x1b[<32;1;200M");
        s.input(c, b"\x1b[<0;1;200m");
        let bottom = right.h();
        assert_eq!(
            String::from_utf8_lossy(&queued(&mut s, 2)),
            format!("\x1b[<0;2;2M\x1b[<32;1;2M\x1b[<32;1;{bottom}M\x1b[<0;1;{bottom}m")
        );
        // Released, a drag from outside is not the pane's.
        s.input(c, b"\x1b[<0;1;2M\x1b[<32;2;2M\x1b[<0;2;2m");
        assert!(queued(&mut s, 2).is_empty());
        Ok(())
    }

    #[test]
    fn a_zoom_elsewhere_lets_go_of_a_button_held_in_the_focus() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str %1 pane str horizontal axis split")?;
        run(&mut s, "str c1 client str %1 pane show")?;
        for pane in [1, 2] {
            s.out(PaneId::of(pane), b"\x1b[?1002h\x1b[?1006h");
        }
        s.input(c, b"\x1b[<0;2;2M");
        assert_eq!(queued(&mut s, 1), b"\x1b[<0;2;2M");
        // Another's zoom moves its focus: the drag and the release past
        // the bottom go to neither pane.
        run(&mut s, "%2 zoom")?;
        s.input(c, b"\x1b[<32;1;200M\x1b[<0;1;200m");
        assert!(queued(&mut s, 1).is_empty() && queued(&mut s, 2).is_empty());
        Ok(())
    }

    #[test]
    fn the_mouse_is_reported_only_while_no_screen_of_the_clients_is_open() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str %1 pane str horizontal axis split")?;
        run(&mut s, "str c1 client str %2 pane show")?;
        s.out(PaneId::of(2), b"\x1b[?1003h\x1b[?1006h");
        assert_eq!(asked_mouse_level(&s, c), Some(1003));
        let right = rect_of(&s, c, 2).ok_or("no rect for %2")?;
        let press = format!("\x1b[<0;{};2M", right.x() + 2);
        // The column and a prompt have the keys: no reporting, and a
        // report in flight is dropped.
        for (open, close) in [(&b"\x02"[..], &b"\x1b"[..]), (b"\x02e", b"\x1b")] {
            let name = String::from_utf8_lossy(open);
            s.input(c, open);
            assert_eq!(asked_mouse_level(&s, c), Some(0), "{name:?}");
            s.input(c, press.as_bytes());
            assert!(queued(&mut s, 2).is_empty(), "{name:?}");
            s.input(c, close);
            s.escape(c);
            assert_eq!(mode(&s, c), "normal", "{name:?}");
            assert_eq!(asked_mouse_level(&s, c), Some(1003), "{name:?}");
        }
        // Focus on a pane whose program did not ask: none.
        run(&mut s, "str c1 client str %1 pane show")?;
        assert_eq!(asked_mouse_level(&s, c), Some(0));
        s.input(c, b"\x1b[<0;2;2M");
        assert!(queued(&mut s, 1).is_empty() && queued(&mut s, 2).is_empty());
        // Zoomed, the pane fills the screen, and its cells are the screen's.
        run(&mut s, "%2 zoom")?;
        s.input(c, b"\x1b[<0;2;2M");
        assert_eq!(queued(&mut s, 2), b"\x1b[<0;2;2M");
        Ok(())
    }

    /// A chooser about a pane that closes closes too, on the server and the
    /// client, saying why: its entries never act on another pane.
    #[test]
    fn a_list_whose_item_disappears_closes_and_never_retargets() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str %1 pane str horizontal axis split")?;
        run(&mut s, "str c1 client str %2 pane show")?;
        s.input(c, b"\x02as");
        assert!(mode(&s, c).starts_with("list "), "{}", mode(&s, c));
        run(&mut s, "str %2 pane kill")?;
        assert_eq!(mode(&s, c), "normal");
        assert_eq!(screen(&s, c), Screen::default());
        assert!(notice(&s, c).contains("%2 is gone"), "{}", notice(&s, c));
        Ok(())
    }

    /// A workspace named by its name is held by its ID: renamed while a
    /// prompt or a confirmation for it is open, it is the same workspace,
    /// and they stay open on it.
    #[test]
    fn an_overlay_holds_a_workspace_named_by_name_through_a_rename() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "+1 str main rename")?;
        run(&mut s, "str c1 client str main workspace rename-prompt")?;
        run(&mut s, "str main workspace str other rename")?;
        assert_eq!(mode(&s, c), "prompt main|", "{}", notice(&s, c));
        s.input(c, b"\x7f\x7f\x7f\x7fnew\r");
        assert_eq!(
            s.parts.workspaces().first().and_then(|w| w.name()),
            Some("new")
        );
        run(&mut s, "str c1 client str new workspace confirm-close")?;
        run(&mut s, "str new workspace str again rename")?;
        assert_eq!(mode(&s, c), "list close workspace +1 new? 0");
        s.input(c, b"y");
        assert!(
            s.parts
                .workspaces()
                .iter()
                .all(|w| w.name() != Some("again"))
        );
        Ok(())
    }

    #[test]
    fn choosers_mark_the_current_item_and_start_on_it() -> Outcome {
        let (mut s, c) = session()?;
        run(
            &mut s,
            "opts 80 24 size str +1 workspace in str second name new-tab",
        )?;
        run(&mut s, "str c1 client str @2 tab show")?;
        s.input(c, b"\x02tg");
        let Some(Mode::Panel(Panel::List(list))) = s.views.get(&c).map(|v| &v.mode) else {
            return Err(mode(&s, c));
        };
        assert_eq!((list.current, list.selected), (Some(1), 1));
        assert!(
            list.items.iter().all(|i| i.contains("— %")),
            "{:?}",
            list.items
        );
        Ok(())
    }

    #[test]
    fn a_prompt_edits_one_line() -> Outcome {
        let (mut s, c) = session()?;
        s.input(c, b"\x02e");
        s.input(c, "abc界".as_bytes());
        assert_eq!(mode(&s, c), "prompt abc界|");
        s.input(c, b"\x1b[D\x1b[D\x7fX");
        assert_eq!(mode(&s, c), "prompt aX|c界");
        s.input(c, b"\x1b[3~");
        assert_eq!(mode(&s, c), "prompt aX|界");
        // Home and End; Ctrl keys type and do nothing.
        s.input(c, b"\x1b[HY\x1b[FZ");
        assert_eq!(mode(&s, c), "prompt YaX界Z|");
        s.input(c, b"\x01\x05\x15\x03\x07");
        assert_eq!(mode(&s, c), "prompt YaX界Z|");
        s.input(c, b"\x7f\x7f\x7f\x7f\x7f");
        assert_eq!(mode(&s, c), "prompt |");
        // A paste of two lines is refused whole, and said, a blank one too;
        // of one line, its line end left out, typed.
        s.input(c, b"\x1b[200~one\ntwo\x1b[201~");
        assert_eq!(mode(&s, c), "prompt |");
        s.input(c, b"\x1b[200~one\n\n\x1b[201~");
        assert_eq!(mode(&s, c), "prompt |");
        let notice = s.views.get(&c).and_then(|v| v.notice.clone());
        assert!(notice.is_some_and(|n| n.error && n.text.contains("one line")));
        s.input(c, b"\x1b[200~one\n\x1b[201~");
        assert_eq!(mode(&s, c), "prompt one|");
        escape(&mut s, c);
        assert_eq!(mode(&s, c), "normal");
        assert_eq!(screen(&s, c), Screen::default());
        Ok(())
    }
}
