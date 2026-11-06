//! What an attached client sends: typed actions (`fux_wire::act`), routed to
//! its overlay or its focused pane. The client reads its
//! own keyboard and runs its own bindings; a key reaches the server only
//! as literal input for the pane it focuses.
use crate::link::Typed;
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
            shown |= self.act(client, act, now);
        }
        // Keys a pane's program reads show when its output does: nothing
        // to paint or settle until then.
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
    fn act(&mut self, client: ClientId, act: Act, now: Instant) -> bool {
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
            Act::Key(stroke) => return self.key(client, stroke, now),
            Act::Paste(text) => self.paste(client, text, now),
            // A focus change or a mouse report for the program shows
            // nothing until it answers, but its refusal.
            Act::Focus(gained) => return self.focus_event(client, gained, now),
            Act::Mouse(event) => return self.mouse(client, event, now),
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
                    let opening = view.mode.opening();
                    self.view_event(client, crate::journal::ViewChange::Screen(opening));
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
    fn key(&mut self, client: ClientId, stroke: Keystroke, now: Instant) -> bool {
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
            self.view_event(client, crate::journal::ViewChange::Screen(None));
        }
        // Encoded as its program asked, by its pane.
        let refused = self.type_in(client, Typed::Key(stroke), now);
        shown || refused
    }

    /// Types into the client's focused pane: a key or a paste, as its
    /// program asked (`put_in`).
    fn type_in(&mut self, client: ClientId, typed: Typed, now: Instant) -> bool {
        let Some(pane) = self.focused(client) else {
            return false;
        };
        self.typed(client);
        self.put_in(client, pane, typed, now)
    }

    /// Sends pane `pane` what client `client` typed, which the pane encodes
    /// as its program asked, or refuses, telling `client` why
    /// (`link::FromPane::Refused`). Its own pane would hand it back what it
    /// typed, again and again: refused here, as one this client runs in;
    /// whether it was.
    fn put_in(
        &mut self,
        client: ClientId,
        pane: fux_wire::id::PaneId,
        typed: Typed,
        now: Instant,
    ) -> bool {
        if self
            .views
            .get(&client)
            .is_some_and(|v| v.runs_in == Some(pane))
        {
            let why = format!("{pane} runs this client: what it types there is not sent");
            self.notify_to(client, why, true);
            return true;
        }
        self.tell_pane(pane, crate::link::ToPane::Typed { client, typed }, now);
        false
    }

    fn paste(&mut self, client: ClientId, text: String, now: Instant) {
        let Some(view) = self.views.get_mut(&client) else {
            return;
        };
        // Pastes go to the pane only.
        if !view.mode.is_normal() {
            return;
        }
        view.notice = None;
        // Text its connection kept goes from it to the pane.
        match self.body_token() {
            Some((body, token)) => {
                let runs_in = self.views.get(&client).and_then(|v| v.runs_in);
                if let Some(pane) = self.focused(client).filter(|p| runs_in != Some(*p)) {
                    self.deliveries.push((body, token, vec![pane]));
                }
                let _ = self.type_in(client, Typed::Body(token), now);
            }
            None => {
                let _ = self.type_in(client, Typed::Paste(text), now);
            }
        }
    }

    /// The outer terminal gained or lost focus: the client's focused pane
    /// hears of it, if it asked and its program takes input: a pane whose
    /// program ended is not told, nor its viewer refused.
    fn focus_event(&mut self, client: ClientId, gained: bool, now: Instant) -> bool {
        let Some(pane) = self.focused(client) else {
            return false;
        };
        self.put_in(client, pane, Typed::Focus(gained), now)
    }

    /// A mouse report from the client's terminal, which reports only while
    /// its focused pane's program asked for it (`render::mouse_level`):
    /// moved to the pane's cells, for its pane to encode as the program
    /// asked. fux has no mouse actions of its own: a report outside the
    /// focused pane is dropped, except that the motion and release of a
    /// press made inside are kept to the pane's edge, so the program never
    /// sees a button left down. One that arrives as an overlay opens is
    /// dropped. One the pane refuses is told (`put_in`); whether it was.
    fn mouse(&mut self, client: ClientId, event: MouseEvent, now: Instant) -> bool {
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
        self.put_in(client, pane, Typed::Mouse(event), now)
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
            // Its keys after the prefix: `C-b g` is the map `g`.
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
        let now = std::time::Instant::now();
        (s.host_mut(PaneId::of(pane), |h| h.pane_mut().drain_input(), now)).unwrap_or_default()
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
            .map_or(0, |p| p.size().cols())
    }

    /// How many tabs the first workspace has.
    fn tabs(s: &Session) -> Option<usize> {
        s.parts.workspaces().first().map(|w| w.tabs().count())
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
    const NEW_TAB: &str = "(new-tab (in (size (options) 80 24) (workspace \"+1\")))";

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

    /// Each says the kind of screen it leaves shown, none once it closes.
    #[test]
    fn openings_replacements_and_closes_invalidate_from_the_prior_cursor() -> Outcome {
        use crate::journal::Reader;
        use fux_wire::events::Actor;
        use fux_wire::inspect::Scope;
        use fux_wire::json::Json;
        let (mut s, c) = session()?;
        s.out(PaneId::of(1), b"first\r\nlast");
        let list = Panel::List(fux_wire::act::List::default());
        for (opening, act, kind) in [
            (1, ScreenAct::Show(Panel::Repeat("first".into())), "repeat"),
            (2, ScreenAct::Show(list), "list"),
            (2, ScreenAct::Hide, ""),
        ] {
            let from = s.parts.journal().end(s.run);
            s.apply(c, vec![Act::Screen { opening, act }]);
            let batch = Reader::new(Scope::Client(c), s.parts.journal(), s.run, Some(from))?
                .batch(s.parts.journal(), true)?;
            assert_eq!(batch.events.len(), 1);
            let event = batch.events.first().ok_or("screen event")?;
            assert_eq!(event.data.get("event"), Some(&Json::str("screen")));
            let shown = (!kind.is_empty()).then(|| Json::str(kind));
            assert_eq!(event.data.get("kind"), Some(&shown.unwrap_or(Json::Null)));
            assert_eq!(event.origin, Actor::Client(c));
            let view = s.views.get(&c).ok_or("view")?;
            assert_eq!(view.opening, opening);
            assert_eq!(view.mode.is_normal(), kind.is_empty());
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
            let ending = &batch.events.first().ok_or("ending")?.data;
            assert_eq!(ending.get("event"), Some(&Json::str("screen")));
            assert_eq!(ending.get("kind"), Some(&Json::Null));
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
            .size()
            .rows();
        s.apply(c, vec![Act::Presentation(presentation)]);
        assert_eq!(
            s.parts
                .panes()
                .get(&PaneId::of(1))
                .ok_or("pane")?
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
        run(
            &mut s,
            "(sequence! (split (axis (pane 1) \"horizontal\")) #inert)",
        )?;
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
            (
                "(sequence! (rename-prompt (client \"c1\") (pane \"%1\")) #inert)",
                "-dev",
                "%1",
            ),
            (
                "(sequence! (rename-prompt (client \"c1\") (tab \"@1\")) #inert)",
                "--",
                "@1",
            ),
            (
                "(sequence! (rename-prompt (client \"c1\") (workspace \"+1\")) #inert)",
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
            let listed = output(&mut s, "(listing (list-session))")?;
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
        let config = "(sequence! (unbind-in \"list\" \"q\") (unbind-in \"list\" \"n\") (bind-in \"list\" \"g\" \"cancel\") (label \"done\") #inert)";
        output_as(&mut s, Some(c), config)?;
        for (bad, said) in [
            (
                "(sequence! (bind-in \"list\" \"Escape\" \"first\") #inert)",
                "cannot be bound",
            ),
            (
                "(sequence! (bind-in \"list\" \"g\" \"nowhere\") #inert)",
                "is not up, down",
            ),
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
    #[cfg(unix)]
    const CONFIRM: &str = concat!(
        "(require! \"",
        env!("CARGO_MANIFEST_DIR"),
        "/../recipes/confirm.grift\")"
    );

    /// A question's or a prompt's action is checked as it opens: one with
    /// a mistake opens nothing.
    // Unix alone: `/bin/sh`, `/bin/cat`, or `/` as an absolute directory.
    #[cfg(unix)]
    #[test]
    fn an_action_is_checked_as_its_screen_opens() -> Outcome {
        let (mut s, c) = session()?;
        for (action, said) in [
            ("\"drop\"".to_owned(), "callback code"),
            (
                "(list () (code! (lambda! () #inert)) (code! (lambda! () #inert)))".to_owned(),
                "a callback is",
            ),
        ] {
            let open = format!("{CONFIRM} (confirm (me) \"sure?\" {action})");
            let refused = output_as(&mut s, Some(c), &open).err().unwrap_or_default();
            assert!(refused.contains(said), "{refused}");
            assert_eq!(mode(&s, c), "normal");
        }
        // Grift resolves a name when it runs: a screen opens on a callback
        // naming nothing, and Enter says which name.
        let open = format!("{CONFIRM} (confirm (me) \"late\" (code! (lambda! () (nosuch))))");
        output_as(&mut s, Some(c), &open)?;
        assert_eq!(mode(&s, c), "list late 0");
        s.input(c, b"y");
        let failed = notice(&s, c);
        assert!(
            failed.contains("unbound-symbol") && failed.contains("nosuch"),
            "{failed}"
        );
        assert_eq!(mode(&s, c), "normal");
        // A prompt's start is a line as typing makes one: no line end.
        let open = "(sequence! (prompt (me) \"t\" (apply string-append (list \"a\" (code-point-to-string 10) \"b\")) (code! (lambda! (argument-140) #inert))) #inert)";
        let refused = output_as(&mut s, Some(c), open).err().unwrap_or_default();
        assert!(refused.contains("bad-value"), "{refused}");
        assert_eq!(mode(&s, c), "normal");
        Ok(())
    }

    /// A confirmation's one row, Yes, chosen by `y` or Enter, runs its
    /// callback on the pane it captured; `n` and Escape cancel, and the
    /// list's row actions, `r` and `x`, refuse its row: it is no pane, tab
    /// or workspace.
    // Unix alone: `/bin/sh`, `/bin/cat`, or `/` as an absolute directory.
    #[cfg(unix)]
    #[test]
    fn a_confirmation_runs_its_callback_by_y_or_enter_alone() -> Outcome {
        let (mut s, c) = session()?;
        let ask = &format!(
            "{CONFIRM} (confirm (me) \"sure?\" (list (list (pane 1)) (code! (lambda! (p) (rename p \"yes\")))))"
        );
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
                assert!(said.contains("not boolean"), "{key}: {said}");
            }
            assert_eq!(
                output(
                    &mut s,
                    "(sequence! (define! value-148 (pane 1)) \"name\" (get value-148 \"name\"))"
                )?,
                "none\n",
                "{key}"
            );
        }
        for key in ["y", "\r"] {
            run(&mut s, "(sequence! (rename (pane 1) \"no\") #inert)")?;
            output_as(&mut s, Some(c), ask)?;
            s.input(c, key.as_bytes());
            assert_eq!(
                output(
                    &mut s,
                    "(sequence! (define! value-158 (pane 1)) \"name\" (get value-158 \"name\"))"
                )?,
                "yes\n",
                "{key:?}: {}",
                notice(&s, c)
            );
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
        run(
            &mut s,
            "(sequence! (define! value-164 (client \"c1\")) (choose-among value-164 (tab (focused value-164))) #inert)",
        )?;
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
        run(
            &mut s,
            "(sequence! (define! value-175 (client \"c1\")) (choose-among value-175 (tab (focused value-175))) #inert)",
        )?;
        s.input(c, b"\x1b[HJ");
        assert!(mode(&s, c).ends_with(" 0"));
        s.input(c, b"j");
        assert!(mode(&s, c).ends_with(" 1"));
        s.input(c, b"q");
        run(
            &mut s,
            "(sequence! (confirm-close (client \"c1\") (tab \"@2\")) #inert)",
        )?;
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
        run(
            &mut s,
            "(sequence! (define! value-189 (client \"c1\")) (choose-among value-189 (tab (focused value-189))) #inert)",
        )?;
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

    /// A map, and one that repeats, bound here rather than by default.
    fn with_maps(s: &mut Session) -> Outcome {
        run(
            s,
            "(sequence! (bind-key \"C-b g n\" (code! (lambda! () (sequence! (new-tab (directory (options) \"/\")) #inert)))) (label \"new tab\") (group \"Tabs\") #inert)",
        )?;
        run(
            s,
            "(sequence! (bind-key \"C-b y l\" (code! (lambda! () (sequence! (move-border (focused (me)) \"right\" 1) #inert)))) (group \"Grow\") #inert)",
        )?;
        run(
            s,
            "(sequence! (bind-key \"C-b y h\" (code! (lambda! () (sequence! (move-border (focused (me)) \"left\" 1) #inert)))) (group \"Grow\") #inert)",
        )?;
        run(s, "(sequence! (repeat \"C-b y\") #inert)")
    }

    /// A map's key opens it, its column headed by the keys so far; a key
    /// it does not bind keeps it open, saying so; its binding runs on the
    /// next key; the prefix inside it goes to the pane; and Enter on its
    /// entry in the column opens it too.
    // Unix alone: `/bin/sh`, `/bin/cat`, or `/` as an absolute directory.
    #[cfg(unix)]
    #[test]
    fn a_map_runs_its_binding_on_the_next_key() -> Outcome {
        let (mut s, c) = session()?;
        with_maps(&mut s)?;
        s.input(c, b"\x02g");
        assert_eq!(mode(&s, c), "column g 0");
        let text = screen_text(&s, c)?;
        assert!(text.contains("C-b g") && text.contains("Tabs"), "{text}");
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
        let prefix = fux::keymap::keys("C-b")?;
        let at = (s
            .test_keymap
            .keys
            .column(&prefix, 0)
            .ok_or("a column")?
            .entries)
            .iter()
            .position(|e| e.key.to_string() == "g")
            .ok_or("the map is listed")?;
        s.input(c, b"\x02");
        s.input(c, &downs(at));
        s.input(c, b"\r");
        assert_eq!(mode(&s, c), "column g 0");
        Ok(())
    }

    #[test]
    fn a_map_that_repeats_keeps_its_keys_until_enter_or_esc() -> Outcome {
        let (mut s, c) = session()?;
        with_maps(&mut s)?;
        run(&mut s, "(split (axis (pane \"%1\") \"horizontal\"))")?;
        run(
            &mut s,
            "(sequence! (show (client \"c1\") (pane \"%1\")) #inert)",
        )?;
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
        with_maps(&mut s)?;
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
        run(&mut s, "(split (axis (pane \"%1\") \"horizontal\"))")?;
        run(
            &mut s,
            "(sequence! (show (client \"c1\") (pane \"%1\")) #inert)",
        )?;
        let start = pane_width(&s, 1);
        run(
            &mut s,
            "(sequence! (bind-key \"C-b r Enter\" (code! (lambda! () (sequence! (move-border (focused (me)) \"right\" 1) #inert)))) #inert)",
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
        let ls = output(s, "(listing (list-session))")?;
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
        run(&mut s, "(split (axis (pane \"%1\") \"horizontal\"))")?;
        run(&mut s, "(split (axis (pane \"%2\") \"horizontal\"))")?;
        run(
            &mut s,
            "(sequence! (show (client \"c1\") (pane \"%3\")) #inert)",
        )?;
        assert_eq!(pane_order(&mut s)?, ["%1", "%2", "%3"]);
        s.input(c, &prefixed("mhh"));
        escape(&mut s, c);
        assert_eq!(mode(&s, c), "normal");
        assert_eq!(pane_order(&mut s)?, ["%3", "%1", "%2"]);
        Ok(())
    }

    #[test]
    fn the_tab_map_reorders_in_a_repeat_mode() -> Outcome {
        let (mut s, c) = session()?;
        run(
            &mut s,
            "(new-tab (name (in (size (options) 80 24) (workspace \"+1\")) \"two\"))",
        )?;
        run(
            &mut s,
            "(new-tab (name (in (size (options) 80 24) (workspace \"+1\")) \"three\"))",
        )?;
        run(
            &mut s,
            "(sequence! (show (client \"c1\") (tab \"@1\")) #inert)",
        )?;
        s.input(c, &prefixed("tmll"));
        assert_eq!(mode(&s, c), "repeat REORDER  h l · Esc");
        escape(&mut s, c);
        let order: Vec<String> = (s.parts.workspaces().first())
            .map(|w| {
                w.tabs()
                    .map(|t| crate::workspace::Child::id(t).to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(order, ["@2", "@3", "@1"]);
        Ok(())
    }

    // Unix alone: `/bin/sh`, `/bin/cat`, or `/` as an absolute directory.
    #[cfg(unix)]
    #[test]
    fn keys_after_the_prefix_match_as_typed() -> Outcome {
        let (mut s, c) = session()?;
        run(
            &mut s,
            "(sequence! (bind-key \"C-b V\" (code! (lambda! () (sequence! (new-tab (directory (options) \"/\")) #inert)))) #inert)",
        )?;
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

    // Unix alone: `/bin/sh`, `/bin/cat`, or `/` as an absolute directory.
    #[cfg(unix)]
    #[test]
    fn a_binding_wins_over_the_columns_own_keys() -> Outcome {
        let (mut s, c) = session()?;
        // Unbound, Up and Down move the column's selection.
        s.input(c, &prefixed("\x1b[B\x1b[B\x1b[A"));
        assert_eq!(mode(&s, c), "column 1");
        escape(&mut s, c);
        run(
            &mut s,
            "(sequence! (bind-key \"C-b Up\" (code! (lambda! () (sequence! (new-tab (directory (options) \"/\")) #inert)))) #inert)",
        )?;
        s.input(c, &prefixed("\x1b[A"));
        assert_eq!(tabs(&s), Some(2));
        assert_eq!(mode(&s, c), "normal");
        // Down, still unbound, still moves.
        s.input(c, &prefixed("\x1b[B"));
        assert_eq!(mode(&s, c), "column 1");
        escape(&mut s, c);
        // The prefix twice sends it, until its map binds it.
        let _ = queued_focused(&mut s, c);
        s.input(c, &[0x02, 0x02]);
        assert_eq!(queued_focused(&mut s, c), b"\x02");
        run(
            &mut s,
            "(sequence! (bind-key \"C-b C-b\" (code! (lambda! () (sequence! (new-tab (directory (options) \"/\")) #inert)))) #inert)",
        )?;
        s.input(c, &[0x02, 0x02]);
        assert_eq!(tabs(&s), Some(3));
        assert!(queued_focused(&mut s, c).is_empty());
        // A digit, a function key and a chord bind as letters do.
        for key in ["1", "F5", "M-n"] {
            run(
                &mut s,
                &format!(
                    "(sequence! (bind-key {} (code! (lambda! () (sequence! (new-tab (directory (options) \"/\")) #inert)))) #inert)",
                    fux::script::quoted(&format!("C-b {key}"))
                ),
            )?;
        }
        s.input(c, &prefixed("1"));
        s.input(c, &prefixed("\x1b[15~"));
        s.input(c, &prefixed("\x1bn"));
        assert_eq!(tabs(&s), Some(6));
        Ok(())
    }

    // Unix alone: `/bin/sh`, `/bin/cat`, or `/` as an absolute directory.
    #[cfg(unix)]
    #[test]
    fn a_key_bound_without_the_prefix_runs_at_once_and_reaches_no_pane() -> Outcome {
        let (mut s, c) = session()?;
        run(
            &mut s,
            "(sequence! (bind-key \"M-t\" (code! (lambda! () (sequence! (new-tab (directory (options) \"/\")) #inert)))) #inert)",
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
        Ok(())
    }

    /// A session with room for every command to act: two workspaces, the
    /// first with two tabs and a workspace, its first tab with three panes,
    /// 1 | (2 / 3), the client on %2.
    fn busy() -> Result<(Session, ClientId), String> {
        let (mut s, c) = session()?;
        run(&mut s, "(split (axis (pane \"%1\") \"horizontal\"))")?;
        run(&mut s, "(split (axis (pane \"%2\") \"vertical\"))")?;
        run(&mut s, NEW_TAB)?;
        run(
            &mut s,
            "(new-tab (size (in (options) \"new-workspace\") 80 24))",
        )?;
        run(
            &mut s,
            "(new-tab (size (in (in (options) (workspace 1)) \"new-workspace\") 80 24))",
        )?;
        run(
            &mut s,
            "(sequence! (show (client \"c1\") (pane \"%2\")) #inert)",
        )?;
        Ok((s, c))
    }

    /// What a user can tell apart: every workspace, tab, pane and client,
    /// what the server draws for the client, its notice, and the screen
    /// its client has open.
    #[cfg(unix)]
    fn state(s: &mut Session, c: ClientId) -> Result<String, String> {
        // Opening the prefix panel and leaving it are real journal events;
        // direct evaluation has a different history, with the same state.
        let ls = output(s, "(listing (list-session))")?
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

    /// With one workspace at the top, the keys into a workspace in it and
    /// out again come back where they started: out shows what the holding
    /// workspace showed before.
    #[test]
    fn into_a_workspace_and_out_again_is_a_round_trip() -> Outcome {
        let (mut s, c) = session()?;
        run(
            &mut s,
            "(new-tab (size (in (in (options) (workspace 1)) \"agent1\") 80 24))",
        )?;
        let at = |s: &Session| s.place(c).map(|p| (p.ws, p.tab));
        let start = at(&s);
        s.input(c, &prefixed("wj"));
        let agent1 = (fux_wire::id::WsId::of(2), fux_wire::id::TabId::of(2));
        assert_eq!(at(&s), Some(agent1));
        // Nothing is in this one.
        s.input(c, &prefixed("wj"));
        assert!(notice(&s, c).contains("none is in it"), "{}", notice(&s, c));
        s.input(c, &prefixed("wk"));
        assert_eq!(at(&s), start);
        Ok(())
    }

    /// Out of a workspace at the top has nowhere to go: refused, the client
    /// where it was, though it showed another top workspace before.
    #[test]
    fn out_of_a_workspace_at_the_top_is_refused() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "(new-tab (size (in (options) \"proj\") 80 24))")?;
        run(
            &mut s,
            "(sequence! (show (client \"c1\") (workspace \"proj\")) (show (client \"c1\") (pane 1)) #inert)",
        )?;
        let start = s.place(c).map(|p| (p.ws, p.tab));
        s.input(c, &prefixed("wk"));
        assert_eq!(s.place(c).map(|p| (p.ws, p.tab)), start);
        assert!(notice(&s, c).contains("none holds it"), "{}", notice(&s, c));
        Ok(())
    }

    /// Typing a default binding's keys does what running its program as the
    /// client does, and the program runs; a repeating one also enters its
    /// mode.
    // Unix alone: `/bin/sh`, `/bin/cat`, or `/` as an absolute directory.
    #[cfg(unix)]
    #[test]
    fn every_default_binding_runs_its_program() -> Outcome {
        let keymap = fux::script::default_state()
            .map(|state| state.keys)
            .map_err(|e| e.to_string())?;
        let bindings: Vec<_> = (keymap.root.all().into_iter())
            .filter_map(|(path, entry)| match entry {
                fux::keymap::Entry::Run(binding) => Some((path, binding)),
                fux::keymap::Entry::Map(_) => None,
            })
            .collect();
        assert!(!bindings.is_empty());
        let mut refused = Vec::new();
        for (path, binding) in bindings {
            let map = path.split_last().map_or(&[][..], |(_, map)| map);
            let repeat = keymap.root.map(map).is_some_and(|m| m.repeat);
            // Every default key follows the prefix, C-b.
            let after = path.get(1..).unwrap_or_default();
            let keys: String = after
                .iter()
                .map(fux_vt::keys::KeyPress::to_string)
                .collect();
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
            if let Err(error) = crate::session::testing::output_job(
                &mut ran,
                Some(d),
                &fux::screens::Job::from(binding.code.clone()),
            ) {
                refused.push(keys.clone());
                // Leaving a repeat mode clears what its keys said.
                if !repeat {
                    ran.notify_to(d, error, true);
                }
            }
            assert_eq!(state(&mut typed, c)?, state(&mut ran, d)?, "{keys}");
        }
        // Refused as the session stands: no pane that way, nothing before
        // the first tab or workspace, none holding the first workspace,
        // nothing but a shell to signal, and no readable nearby cwd in
        // these processless panes.
        refused.sort();
        assert_eq!(
            refused,
            [
                "ak", "c", "k", "l", "mk", "ml", "s", "tmh", "tn", "v", "wk", "wmh", "wn"
            ]
        );
        Ok(())
    }

    // Unix alone: `/bin/sh`, `/bin/cat`, or `/` as an absolute directory.
    #[cfg(unix)]
    #[test]
    fn every_chooser_of_the_keys_builds_its_rows_from_one_topology_observation() -> Outcome {
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
        // A workspace that closed, or a pane to move to, opens none.
        let gone = r#"(define! w (workspace (new-tab (size (directory (in (options) "gone") "/") 80 24)))) (kill w)"#;
        for (code, refused) in [
            (
                format!("{gone} (choose-among (me) w)"),
                "chooser target is absent",
            ),
            (
                format!("{gone} (choose-move (me) (pane 2) w)"),
                "chooser target is absent",
            ),
            (
                "(sequence! (choose-move (me) (pane 2) (pane 2)) #inert)".to_owned(),
                "a tab or workspace",
            ),
        ] {
            let code = code.as_str();
            let (mut s, c) = busy()?;
            let error = output_as(&mut s, Some(c), code).err().unwrap_or_default();
            assert!(error.contains(refused), "{code}: {error}");
            assert!(
                s.views.get(&c).is_some_and(|v| v.mode.is_normal()),
                "{code}"
            );
        }
        for (code, captures) in [
            (
                "(sequence! (choose-among (me) (tab (focused (me)))) #inert)",
                0,
            ),
            (
                "(sequence! (choose-among (me) (workspace (focused (me)))) #inert)",
                0,
            ),
            (
                "(sequence! (choose-move (me) (pane 2) (tab (pane 2))) #inert)",
                1,
            ),
            (
                "(sequence! (choose-move (me) (pane 2) (workspace (pane 2))) #inert)",
                1,
            ),
            ("(sequence! (choose-pane (me) (pane 2)) #inert)", 1),
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
            };
            // In a client's dictionary, as a key's program is.
            let mut machine = fux::screens::Job::from(code.to_owned())
                .machine(None, None)
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            let mut host = Host::new(context, Some(&mut state), &mut out);
            fux::script::run(&mut machine, &mut host, &mut asks, None)
                .map_err(|e| e.to_string())?;
            let Some(Screen::List(list)) = asks.opened else {
                return Err(format!("{code}: no list"));
            };
            assert!(!list.items.is_empty());
            if code.ends_with("workspace choose-among") {
                // Every workspace on the server, +3 under +1 that holds it
                // and the client's, none an empty row.
                let labels: Vec<&str> = list.items.iter().map(|i| i.label.as_str()).collect();
                let [one, three, two] = labels.as_slice() else {
                    return Err(format!("{labels:?}"));
                };
                assert!(
                    one.starts_with("* +1 ") && one.contains("(workspace 1)"),
                    "{labels:?}"
                );
                assert!(three.starts_with("    +3 "), "{labels:?}");
                assert!(two.starts_with("  +2 "), "{labels:?}");
            }
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
    fn chooser_callbacks_act_on_the_selected_destination() -> Outcome {
        for (open, keys, observe, expected) in [
            (
                "(sequence! (choose-among (me) (tab (focused (me)))) #inert)",
                "j\r",
                "(string (tab (focused (me))))",
                "@2\n",
            ),
            (
                "(sequence! (choose-among (me) (workspace (focused (me)))) #inert)",
                "j\r",
                "(string (workspace (focused (me))))",
                "+3\n",
            ),
            (
                "(sequence! (choose-move (me) (pane 2) (tab (pane 2))) #inert)",
                "j\r",
                "(string (tab (pane 2)))",
                "@2\n",
            ),
            (
                "(sequence! (choose-move (me) (pane 2) (workspace (pane 2))) #inert)",
                "j\r",
                "(string (workspace (pane 2)))",
                "+2\n",
            ),
            (
                "(sequence! (choose-pane (me) (pane 2)) #inert)",
                "\r",
                "(sequence! (fold (lambda! (accumulator-580 item-576) (let! ((() accumulator-580)) (sequence! (print (string item-576)) (list)))) (list) (panes (tab 1))) #inert)",
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
            (
                "(sequence! (choose-among (me) (tab (focused (me)))) #inert)",
                "@1",
                "tab",
            ),
            (
                "(sequence! (choose-among (me) (workspace (focused (me)))) #inert)",
                "+1",
                "workspace",
            ),
            (
                "(sequence! (choose-move (me) (pane 2) (tab (pane 2))) #inert)",
                "@1",
                "tab",
            ),
            (
                "(sequence! (choose-move (me) (pane 2) (workspace (pane 2))) #inert)",
                "+1",
                "workspace",
            ),
            (
                "(sequence! (choose-pane (me) (pane 2)) #inert)",
                "%1",
                "pane",
            ),
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
                output(
                    &mut s,
                    &format!(
                        "(sequence! \"name\" (get {} \"name\"))",
                        crate::test_reference(target)
                    )
                )?,
                "destination\n",
                "{open}"
            );
            assert_eq!(
                output(
                    &mut s,
                    "(sequence! (define! value-622 (pane 2)) \"name\" (get value-622 \"name\"))"
                )?,
                "none\n",
                "{open}"
            );
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
                output(
                    &mut s,
                    &format!(
                        "(sequence! \"name\" (get {} \"name\"))",
                        crate::test_reference(target)
                    )
                )
                .is_err(),
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
            "(sequence! (bind-in \"list\" \"R\" \"confirm-close\") #inert)",
            "(sequence! (bind-in \"list\" \"R\" \"rename\") #inert)",
            "(sequence! (bind-action \"R\" \"nosuch\") #inert)",
            "(sequence! (bind-action \"Escape\" (code! (lambda! (argument-646 argument-647) #inert))) #inert)",
        ] {
            assert!(output_as(&mut s, Some(c), bad).is_err(), "{bad}");
        }
        output_as(
            &mut s,
            Some(c),
            "(sequence! (bind-action \"R\" (code! (lambda! (argument-654 argument-655) (sequence! (print argument-655) #inert)))) (label \"test\") #inert)",
        )?;
        run(
            &mut s,
            "(sequence! (choose (client \"c1\") \"hosts\" (list (list \"%1 kill\" \"%1 kill\")) () (list 0) (code! (lambda! (argument-677 argument-678) (sequence! (print argument-678) #inert)))) #inert)",
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
            ("()", "()", "()", true),
            ("()", "(list 0)", "()", false),
            ("(list (list 7 \"seven\"))", "()", "()", false),
            ("(list (list 7 \"seven\"))", "()", "(list -1)", false),
            ("(list (list 7 \"seven\"))", "()", "(list 1)", false),
        ] {
            let code = format!(
                "(sequence! (choose (me) \"title\" {} {} {} (code! (lambda! (argument-106 argument-107) #inert))) #inert)",
                crate::test_reference(rows),
                crate::test_reference(current),
                crate::test_reference(selected)
            );
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
            "(sequence! (choose (me) \"title\" (list (list 7 \"seven\") (list 8 \"eight\")) (list 1) (list 0) (code! (lambda! (argument-772 argument-773) #inert))) #inert)",
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
            "(sequence! (rename (tab 1) \"shared\") (new-tab (in (size (options) 80 24) (workspace \"+1\"))) #inert)",
        )?;
        output_as(
            &mut s,
            Some(c),
            "(sequence! (choose-among (me) (tab (focused (me)))) #inert)",
        )?;
        run(
            &mut s,
            "(sequence! (kill (tab 1)) (new-tab (name (in (size (options) 80 24) (workspace \"+1\")) \"shared\")) #inert)",
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
        run(&mut s, "(split (axis (pane \"%1\") \"horizontal\"))")?;
        run(
            &mut s,
            "(sequence! (show (client \"c1\") (pane \"%2\")) #inert)",
        )?;
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
        run(&mut s, "(split (axis (pane \"%2\") \"vertical\"))")?;
        run(
            &mut s,
            "(sequence! (show (client \"c1\") (pane \"%3\")) #inert)",
        )?;
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
            "(new-tab (program (size (in (options) \"agent\") 80 24) \"/bin/cat\"))",
        )?;
        run(
            &mut s,
            "(sequence! (show (client \"c1\") (pane \"%2\")) #inert)",
        )?;
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
        run(&mut s, "(split (axis (pane \"%1\") \"horizontal\"))")?;
        run(
            &mut s,
            "(sequence! (show (client \"c1\") (pane \"%2\")) #inert)",
        )?;
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
        run(&mut s, "(split (axis (pane \"%1\") \"horizontal\"))")?;
        run(
            &mut s,
            "(sequence! (show (client \"c1\") (pane \"%1\")) #inert)",
        )?;
        for pane in [1, 2] {
            s.out(PaneId::of(pane), b"\x1b[?1002h\x1b[?1006h");
        }
        s.input(c, b"\x1b[<0;2;2M");
        assert_eq!(queued(&mut s, 1), b"\x1b[<0;2;2M");
        // Another's zoom moves its focus: the drag and the release past
        // the bottom go to neither pane.
        run(&mut s, "(sequence! (zoom (pane 2)) #inert)")?;
        s.input(c, b"\x1b[<32;1;200M\x1b[<0;1;200m");
        assert!(queued(&mut s, 1).is_empty() && queued(&mut s, 2).is_empty());
        Ok(())
    }

    #[test]
    fn the_mouse_is_reported_only_while_no_screen_of_the_clients_is_open() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "(split (axis (pane \"%1\") \"horizontal\"))")?;
        run(
            &mut s,
            "(sequence! (show (client \"c1\") (pane \"%2\")) #inert)",
        )?;
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
        run(
            &mut s,
            "(sequence! (show (client \"c1\") (pane \"%1\")) #inert)",
        )?;
        assert_eq!(asked_mouse_level(&s, c), Some(0));
        s.input(c, b"\x1b[<0;2;2M");
        assert!(queued(&mut s, 1).is_empty() && queued(&mut s, 2).is_empty());
        // Zoomed, the pane fills the screen, and its cells are the screen's.
        run(&mut s, "(sequence! (zoom (pane 2)) #inert)")?;
        s.input(c, b"\x1b[<0;2;2M");
        assert_eq!(queued(&mut s, 2), b"\x1b[<0;2;2M");
        Ok(())
    }

    /// A chooser about a pane that closes closes too, on the server and the
    /// client, saying why: its entries never act on another pane.
    #[test]
    fn a_list_whose_item_disappears_closes_and_never_retargets() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "(split (axis (pane \"%1\") \"horizontal\"))")?;
        run(
            &mut s,
            "(sequence! (show (client \"c1\") (pane \"%2\")) #inert)",
        )?;
        s.input(c, b"\x02as");
        assert!(mode(&s, c).starts_with("list "), "{}", mode(&s, c));
        run(&mut s, "(sequence! (kill (pane \"%2\")) #inert)")?;
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
        run(&mut s, "(sequence! (rename (workspace 1) \"main\") #inert)")?;
        run(
            &mut s,
            "(sequence! (rename-prompt (client \"c1\") (workspace \"main\")) #inert)",
        )?;
        run(
            &mut s,
            "(sequence! (rename (workspace \"main\") \"other\") #inert)",
        )?;
        assert_eq!(mode(&s, c), "prompt main|", "{}", notice(&s, c));
        s.input(c, b"\x7f\x7f\x7f\x7fnew\r");
        assert_eq!(
            s.parts.workspaces().first().and_then(|w| w.name()),
            Some("new")
        );
        run(
            &mut s,
            "(sequence! (confirm-close (client \"c1\") (workspace \"new\")) #inert)",
        )?;
        run(
            &mut s,
            "(sequence! (rename (workspace \"new\") \"again\") #inert)",
        )?;
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
            "(new-tab (name (in (size (options) 80 24) (workspace \"+1\")) \"second\"))",
        )?;
        run(
            &mut s,
            "(sequence! (show (client \"c1\") (tab \"@2\")) #inert)",
        )?;
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
