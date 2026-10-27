//! What an attached client sends: typed actions (`crate::act`), routed to
//! its overlay, its copy mode, or its focused pane. The client reads its
//! own keyboard and runs its own bindings; a key reaches the server only
//! as literal input for the pane it focuses.
use crate::act::{Act, CopyAct, ScreenAct};
use crate::id::ClientId;
use crate::keys::Keystroke;
use crate::keys::mouse::{MouseAction, MouseEvent};
use crate::session::Session;
use crate::view::Mode;

impl Session {
    /// Actions from a client.
    pub fn acts(&mut self, client: ClientId, acts: Vec<Act>) {
        if acts.is_empty() {
            return;
        }
        let changes = self.changes();
        let typist = |s: &Self| s.shown_tab(client).and_then(|t| t.typist);
        let typed = typist(self);
        // Whether any of it may have changed what a client shows.
        let mut shown = false;
        // A client that only watches sends no other acts than its
        // terminal's replies and notices: its own keyboard holds the rest
        // (`Keyboard`), as `read_only` is what it said itself.
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
        self.settle();
    }

    /// One action: whether it may have changed what the client shows.
    fn act(&mut self, client: ClientId, act: Act) -> bool {
        match act {
            Act::Key(stroke) => return self.key(client, stroke),
            Act::Paste(text) => self.paste(client, &text),
            // A focus change or a mouse report for the program shows
            // nothing until it answers, but its refusal.
            Act::Focus(gained) => return self.focus_event(client, gained),
            Act::Mouse(event) => return self.mouse(client, event),
            Act::Reply(reply) => self.terminal_reply(client, reply),
            // The client's screen, drawn as it says; copy mode left, if
            // the client has moved on from it.
            // A notice from before the screen opened or closed is old news;
            // one the screen's keys bring comes after.
            Act::Screen { opening, act } => {
                let Some(view) = self.views.get_mut(&client) else {
                    return false;
                };
                let starts = matches!(
                    &act,
                    ScreenAct::Show(_) | ScreenAct::Copy(CopyAct::Enter { .. })
                );
                let fresh = opening > view.opening;
                if !(fresh && starts || opening == view.opening && !view.mode.is_normal()) {
                    return false;
                }
                if fresh {
                    view.opening = opening;
                    view.mode = Mode::Normal;
                    view.notice = None;
                }
                match act {
                    ScreenAct::Show(panel) => view.mode = Mode::Panel(panel),
                    ScreenAct::Hide => {
                        view.mode = Mode::Normal;
                        view.notice = None;
                    }
                    ScreenAct::Copy(CopyAct::Enter { .. }) if !fresh => return false,
                    ScreenAct::Copy(act) => crate::copy::act(self, client, act),
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
        let shown = match view.mode {
            Mode::Normal => view.notice.take().is_some(),
            Mode::Panel(_) | Mode::Copy(_) => {
                view.mode = Mode::Normal;
                true
            }
        };
        view.dirty |= shown;
        // Encoded as its program asked.
        let refused = self.type_in(client, |screen, out| {
            crate::encode::key_bytes(stroke, screen.key_mode(), out)
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
        pane: crate::id::PaneId,
        write: impl FnOnce(&fux_vt::Screen, &mut Vec<u8>),
    ) -> bool {
        let why = match self.live_pane_mut(pane).map(|mut p| p.push_with(write)) {
            Ok(Ok(())) => return false,
            Ok(Err(crate::pane::Error::InputFull)) => {
                let input = crate::pane::Losses {
                    input: 1,
                    ..Default::default()
                };
                input.told(pane, false).join("; ")
            }
            Ok(Err(refused)) => refused.to_string(),
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
            crate::encode::paste(text, screen.mode(fux_vt::Mode::BracketedPaste), out)
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
        let inside = rect.contains(event.row, event.col);
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
        let Some(reports) = pane_screen.screen().encode_mouse(event) else {
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
    use crate::act::Panel;
    use crate::id::{ClientId, PaneId};
    use crate::screens::Screen;
    use crate::session::Session;
    use crate::session::testing::{output, output_as, run};
    use crate::view::Mode;

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
            Some(Mode::Panel(Panel::Confirm { question, .. })) => format!("confirm {question}"),
            Some(Mode::Copy(_)) => "copy".into(),
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

    /// Every server-side screen action belongs to one opening, including
    /// edits and copy keys already in flight when that opening ended.
    #[test]
    fn stale_screen_actions_cannot_change_a_new_opening() -> Outcome {
        use crate::act::{Act, CopyAct, ScreenAct};
        let (mut s, c) = session()?;
        s.output(PaneId::of(1), b"first\r\nlast");
        let cursor = |s: &Session| {
            s.views.get(&c).and_then(|v| match &v.mode {
                Mode::Copy(copy) => Some(copy.cursor),
                Mode::Normal | Mode::Panel(_) => None,
            })
        };
        let panel = Panel::Prompt {
            title: "prompt".into(),
            before: "unsent work".into(),
            after: String::new(),
            hints: vec![],
        };
        for start in [
            ScreenAct::Show(panel.clone()),
            ScreenAct::Copy(CopyAct::Enter {
                hints: vec![],
                typing: vec![],
            }),
        ] {
            let old = s.views.get(&c).ok_or("view")?.opening.saturating_add(1);
            let new = old.saturating_add(1);
            s.acts(
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
            let copy_cursor = cursor(&s);
            for act in [
                ScreenAct::Hide,
                ScreenAct::Copy(CopyAct::Leave),
                ScreenAct::Copy(CopyAct::Yank),
                ScreenAct::Copy(CopyAct::Top),
                ScreenAct::Show(Panel::Repeat("old panel".into())),
                start.clone(),
            ] {
                s.acts(c, vec![Act::Screen { opening: old, act }]);
                assert_eq!(mode(&s, c), before);
                assert_eq!(cursor(&s), copy_cursor);
                assert_eq!(s.views.get(&c).ok_or("view")?.opening, new);
            }
            s.acts(
                c,
                vec![Act::Screen {
                    opening: new,
                    act: ScreenAct::Hide,
                }],
            );
            assert_eq!(mode(&s, c), "normal");
            s.acts(
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
        run(&mut s, "%1 horizontal split drop")?;
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

    /// A copy opening refused without a focused pane ends on both sides
    /// and reports the missing focus without command-line instructions.
    #[test]
    fn copy_without_a_focused_pane_ends_visibly() -> Outcome {
        let (mut s, c) = session()?;
        s.close_pane(PaneId::of(1), None);
        s.input(c, b"\x02c");
        assert_eq!(mode(&s, c), "normal");
        assert_eq!(screen(&s, c), Screen::default());
        assert_eq!(notice(&s, c), "copy mode: no pane focused");
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
            let listed = output(&mut s, "ls")?;
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
        let config = "str copy str q unbind-in  str copy str g str leave bind-in  str done label";
        output_as(&mut s, Some(c), config)?;
        for (bad, said) in [
            ("str copy str Escape str top bind-in", "cannot be bound"),
            (
                "str copy str g str nowhere bind-in",
                "\"nowhere\" is not left, right",
            ),
        ] {
            let refused = output_as(&mut s, Some(c), bad).err().unwrap_or_default();
            assert!(refused.contains(said), "{refused}");
        }
        s.input(c, b"\x02c");
        let hints = match s.views.get(&c).map(|v| &v.mode) {
            Some(Mode::Copy(copy)) => copy.hints.iter().map(|h| h.keys.clone()).collect(),
            _ => Vec::new(),
        };
        assert!(hints.contains(&"g Escape".to_owned()), "{hints:?}");
        s.input(c, b"q");
        assert_eq!(mode(&s, c), "copy");
        s.input(c, b"g");
        assert_eq!(mode(&s, c), "normal");
        s.input(c, b"\x02c");
        escape(&mut s, c);
        assert_eq!(mode(&s, c), "normal");
        Ok(())
    }

    /// A question's or a prompt's action is checked as it opens: one with
    /// a mistake, or past `ACTION_MAX`, opens nothing.
    #[test]
    fn an_action_is_checked_as_its_screen_opens() -> Outcome {
        let (mut s, c) = session()?;
        let long: String =
            std::iter::repeat_n("1 drop ", crate::screens::ACTION_MAX / 7 + 1).collect();
        for (code, said) in [("nosuch", "in the action"), (long.as_str(), "is past")] {
            let open = format!("me str sure? str '{code}' confirm");
            let refused = output_as(&mut s, Some(c), &open).err().unwrap_or_default();
            assert!(refused.contains(said), "{refused}");
            assert_eq!(mode(&s, c), "normal");
        }
        // A prompt's start is a line as typing makes one: no line end.
        let open = "me str t list str a 10 char str b end glue str drop prompt";
        let refused = output_as(&mut s, Some(c), open).err().unwrap_or_default();
        assert!(refused.contains("bad_value"), "{refused}");
        assert_eq!(mode(&s, c), "normal");
        output_as(
            &mut s,
            Some(c),
            "me str sure? str '%1 str yes rename' confirm",
        )?;
        s.input(c, b"y");
        assert!(output(&mut s, "ls")?.contains("%1 yes"));
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
        run(&mut s, "str c1 client choose-tab")?;
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
        run(&mut s, "str c1 client choose-tab")?;
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
        run(&mut s, "str c1 client choose-tab")?;
        s.input(c, b"\x1b[H");
        s.input(c, &downs(30));
        let Some(Mode::Panel(Panel::List(list))) = s.views.get(&c).map(|v| &v.mode) else {
            return Err(mode(&s, c));
        };
        assert_eq!(list.selected, 30);
        let title = list.title.clone();
        let text = screen_text(&s, c)?;
        for shown in [title.as_str(), "▲", "▼", "Enter select"] {
            assert!(text.contains(shown), "{shown:?} in\n{text}");
        }
        Ok(())
    }

    /// A layer and a repeat mode, bound here rather than by default.
    fn with_layers(s: &mut Session) -> Outcome {
        run(
            s,
            "str 'g n' str 'opts new-tab drop' bind str 'new tab' label str Tabs group",
        )?;
        run(
            s,
            "str 'y l' str 'me focused str right 1 move-border' bind str Grow group repeat",
        )?;
        run(
            s,
            "str 'y h' str 'me focused str left 1 move-border' bind str Grow group repeat",
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
        let at = (s.test_keymap.column(&[], 0).ok_or("a column")?.entries)
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
        run(&mut s, "str %1 pane horizontal split")?;
        run(&mut s, "str c1 client str %1 pane focus-pane")?;
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
        run(&mut s, "str %1 pane horizontal split")?;
        run(&mut s, "str c1 client str %1 pane focus-pane")?;
        let start = pane_width(&s, 1);
        run(
            &mut s,
            "str 'r Enter' str 'me focused str right 1 move-border' bind repeat",
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
        let ls = output(s, "ls")?;
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
        run(&mut s, "str %1 pane horizontal split")?;
        run(&mut s, "str %2 pane horizontal split")?;
        run(&mut s, "str c1 client str %3 pane focus-pane")?;
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
        run(&mut s, "str c1 client str @1 tab show-tab")?;
        s.input(c, &prefixed("tmll"));
        assert_eq!(mode(&s, c), "repeat REORDER  h l · Esc");
        escape(&mut s, c);
        let order: Vec<String> = (s.parts.workspaces().first())
            .map(|w| w.tabs().iter().map(|t| t.id().to_string()).collect())
            .unwrap_or_default();
        assert_eq!(order, ["@2", "@3", "@1"]);
        Ok(())
    }

    #[test]
    fn keys_after_the_prefix_match_as_typed() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str V str 'opts new-tab drop' bind")?;
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
        run(&mut s, "str Up str 'opts new-tab drop' bind")?;
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
        run(&mut s, "str C-b str 'opts new-tab drop' bind")?;
        s.input(c, &[0x02, 0x02]);
        assert_eq!(tabs(&s), Some(3));
        assert!(queued_focused(&mut s, c).is_empty());
        // A digit, a function key and a chord bind as letters do.
        for key in ["1", "F5", "M-n"] {
            run(&mut s, &format!("str {key} str 'opts new-tab drop' bind"))?;
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
        run(&mut s, "str F2 str 'me zoom' bind-root str Session group")?;
        s.input(c, b"\x02");
        let text = screen_text(&s, c)?;
        let headings = text.lines().filter(|l| l.trim() == "Session").count();
        assert_eq!(headings, 2, "{text}");
        Ok(())
    }

    #[test]
    fn a_key_bound_without_the_prefix_runs_at_once_and_reaches_no_pane() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str M-t str 'opts new-tab drop' bind-root")?;
        s.input(c, b"\x1bt");
        assert_eq!(tabs(&s), Some(2));
        assert!(queued_focused(&mut s, c).is_empty());
        // A chord bound to nothing reaches the pane as it was typed.
        s.input(c, b"\x1bl");
        assert_eq!(queued_focused(&mut s, c), b"\x1bl");
        // In copy mode and in the column the key is theirs, not the binding's.
        run(&mut s, "str c1 client copy-mode")?;
        assert_eq!(mode(&s, c), "copy");
        s.input(c, b"\x1bt");
        assert_eq!(tabs(&s), Some(2));
        s.input(c, b"q");
        assert_eq!(mode(&s, c), "normal");
        s.input(c, &prefixed("\x1bt"));
        assert_eq!(tabs(&s), Some(2));
        assert_eq!(notice(&s, c), "C-b M-t is not bound");
        escape(&mut s, c);
        // The column lists it last, and runs it.
        let entries = s.test_keymap.column(&[], 0).ok_or("a column")?.entries;
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
        run(&mut s, "str %1 pane horizontal split")?;
        run(&mut s, "str %2 pane vertical split")?;
        run(&mut s, NEW_TAB)?;
        run(&mut s, "opts 80 24 size new-workspace")?;
        run(&mut s, "str c1 client str %2 pane focus-pane")?;
        Ok((s, c))
    }

    /// What a user can tell apart: every workspace, tab, pane and client,
    /// what the server draws for the client, its notice, and the screen
    /// its client has open.
    fn state(s: &mut Session, c: ClientId) -> Result<String, String> {
        let ls = output(s, "ls")?;
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
        let keymap = crate::script::default_keys().map_err(|e| e.to_string())?;
        let bindings = keymap.bindings.all();
        assert!(!bindings.is_empty());
        let mut refused = Vec::new();
        for (path, binding, repeat) in bindings {
            let keys: String = path.iter().map(crate::keys::KeyPress::to_string).collect();
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
            if let Err(error) = output_as(&mut ran, Some(d), &binding.code) {
                refused.push(keys.clone());
                // Leaving a repeat mode clears what its keys said.
                if !repeat {
                    ran.notify_to(d, error, true);
                }
            }
            assert_eq!(state(&mut typed, c)?, state(&mut ran, d)?, "{keys}");
        }
        // Refused as the session stands: no pane that way, no copy to
        // paste, nothing before the first tab or workspace, nothing but a
        // shell to signal.
        refused.sort();
        assert_eq!(refused, ["ak", "k", "l", "mk", "ml", "p", "tmh", "wmh"]);
        Ok(())
    }

    /// What running `code` as the client of a fresh `busy` session leaves.
    fn ran(code: &str) -> Result<String, String> {
        let (mut s, c) = busy()?;
        if let Err(error) = output_as(&mut s, Some(c), code) {
            s.notify_to(c, error, true);
        }
        state(&mut s, c)
    }

    /// Choosers and confirmations are built by the client from what
    /// the server says (`Open::build`): each entry's fuxth does what the
    /// words for it do.
    #[test]
    fn listed_programs_are_what_their_words_ask() -> Outcome {
        let (mut s, c) = busy()?;
        for (open, programs) in [
            (
                "me choose-tab",
                &["me str @1 tab show-tab", "me str @2 tab show-tab"][..],
            ),
            (
                "me str %2 pane choose-tab-for",
                &[
                    "str %2 pane str @1 tab move-to",
                    "str %2 pane str @2 tab move-to",
                ],
            ),
            (
                "me choose-workspace",
                &[
                    "me str +1 workspace show-workspace",
                    "me str +2 workspace show-workspace",
                ],
            ),
            (
                "me str %2 pane choose-workspace-for",
                &[
                    "str %2 pane str +1 workspace move-to",
                    "str %2 pane str +2 workspace move-to",
                ],
            ),
            (
                "me str %2 pane choose-pane",
                &[
                    "str %2 pane str %1 pane swap-panes",
                    "str %2 pane str %3 pane swap-panes",
                ],
            ),
            ("me str %3 pane confirm-close", &["str %3 pane kill"]),
            ("me str @2 tab confirm-close", &["str @2 tab kill"]),
            (
                "me str +1 workspace confirm-close",
                &["str +1 workspace kill"],
            ),
        ] {
            output_as(&mut s, Some(c), open)?;
            // A confirmation's program is what `y` runs.
            let codes: Vec<String> = match screen(&s, c) {
                Screen::List(list) => list.items.into_iter().map(|i| i.code).collect(),
                mut confirm @ Screen::Confirm(_) => {
                    (confirm.key(&s.test_keymap, crate::keymap::key("y")?.into(), false))
                        .into_iter()
                        .filter_map(|e| match e {
                            crate::screens::Effect::Run(job) => Some(job.code),
                            crate::screens::Effect::Send(_)
                            | crate::screens::Effect::Screen(_)
                            | crate::screens::Effect::Open(_) => None,
                        })
                        .collect()
                }
                other @ (Screen::Keys(_) | Screen::Prompt(_) | Screen::Copy(_)) => {
                    return Err(format!("{open}: {other:?}"));
                }
            };
            assert_eq!(codes.len(), programs.len(), "{open}: {codes:?}");
            for (code, program) in codes.iter().zip(programs) {
                assert_eq!(ran(code)?, ran(program)?, "{open}: {code}");
            }
            escape(&mut s, c);
        }
        // A chooser's `r` and `x` act on the selected item.
        output_as(&mut s, Some(c), "me choose-tab")?;
        s.input(c, b"r");
        assert_eq!(mode(&s, c), "prompt |");
        s.input(c, b"chosen\r");
        output_as(&mut s, Some(c), "me choose-tab")?;
        s.input(c, b"x");
        assert_eq!(mode(&s, c), "confirm close tab @1 chosen?");
        Ok(())
    }

    /// Pane `pane`'s rect on client `c`'s screen.
    fn rect_of(s: &Session, c: ClientId, pane: u32) -> Option<crate::layout::Rect> {
        s.placement(s.views.get(&c)?).rect(PaneId::of(pane))
    }

    /// The mouse tracking client `c`'s terminal is asked for.
    fn asked_mouse_level(s: &Session, c: ClientId) -> Option<u16> {
        crate::render::compose(s, c).map(|g| g.modes.mouse)
    }

    #[test]
    fn a_mouse_report_reaches_the_focused_pane_in_its_cells() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str %1 pane horizontal split")?;
        run(&mut s, "str c1 client str %2 pane focus-pane")?;
        let right = rect_of(&s, c, 2).ok_or("no rect for %2")?;
        assert!(right.x() > 1 && right.y() == 0, "{right:?}");
        // Nothing asked: no reporting, and a report is dropped.
        assert_eq!(asked_mouse_level(&s, c), Some(0));
        let press = |col: u16, row: u16| format!("\x1b[<0;{};{}M", col + 1, row + 1);
        s.input(c, press(right.x() + 3, 2).as_bytes());
        assert!(queued(&mut s, 2).is_empty());
        s.output(PaneId::of(2), b"\x1b[?1000h\x1b[?1006h");
        assert_eq!(asked_mouse_level(&s, c), Some(1000));
        // Moved to the pane's cells.
        s.input(c, press(right.x() + 3, 2).as_bytes());
        assert_eq!(queued(&mut s, 2), b"\x1b[<0;4;3M");
        // On the other pane, or the border between them: dropped.
        s.input(c, press(1, 2).as_bytes());
        s.input(c, press(right.x() - 1, 2).as_bytes());
        assert!(queued(&mut s, 2).is_empty() && queued(&mut s, 1).is_empty());
        // In the encoding the program asked for: here the default.
        s.output(PaneId::of(2), b"\x1b[?1006l");
        s.input(c, press(right.x(), 0).as_bytes());
        assert_eq!(queued(&mut s, 2), [0x1b, b'[', b'M', 32, 33, 33]);
        // A pane below another: moved down too.
        run(&mut s, "str %2 pane vertical split")?;
        run(&mut s, "str c1 client str %3 pane focus-pane")?;
        let below = rect_of(&s, c, 3).ok_or("no rect for %3")?;
        assert!(below.y() > 1, "{below:?}");
        s.output(PaneId::of(3), b"\x1b[?1000h\x1b[?1006h");
        s.input(c, press(below.x() + 1, below.y() + 2).as_bytes());
        assert_eq!(queued(&mut s, 3), b"\x1b[<0;2;3M");
        Ok(())
    }

    #[test]
    fn a_wheel_expansion_is_admitted_or_refused_whole() -> Outcome {
        let expected: Vec<u8> = std::iter::repeat_n(b"\x1b[A", 50)
            .flatten()
            .copied()
            .collect();
        for room in [0, 100, expected.len()] {
            let (mut s, c) = crate::session::testing::attached(101, 20)?;
            let id = PaneId::of(1);
            s.output(id, b"\x1b[?1049h\x1b[?1007h\x1b[?1h\x1b[>1u");
            let filled = crate::pane::INPUT_BYTES.saturating_sub(room);
            s.parts
                .pane_mut(id)
                .ok_or("no %1")?
                .push(vec![0; filled])
                .map_err(|e| e.to_string())?;
            s.input(c, b"\x1b[<80;1;1M");
            let refused = room < expected.len();
            let lost = s.parts.panes().get(&id).ok_or("no %1")?.lost();
            assert_eq!(lost.input, u64::from(refused), "room for {room} bytes");
            if refused {
                assert!(notice(&s, c).starts_with("%1: input dropped (1):"));
            } else {
                assert_eq!(notice(&s, c), "");
            }
            let input = queued(&mut s, 1);
            assert!(
                input
                    .get(..filled)
                    .is_some_and(|b| b.iter().all(|b| *b == 0))
            );
            let arrows = input.get(filled..).ok_or("the queued prefix")?;
            assert_eq!(arrows, if refused { &[] } else { expected.as_slice() });
        }
        Ok(())
    }

    /// A mouse report a full input queue refuses is told, and so is each
    /// key, paste, report and focus change refused after it, the notice
    /// cleared by a key shown again.
    #[test]
    fn a_report_a_full_queue_refuses_is_told_once() -> Outcome {
        let (mut s, c) = session()?;
        s.output(PaneId::of(1), b"\x1b[?1000h\x1b[?1006h\x1b[?1004h");
        let full = vec![0; crate::pane::INPUT_BYTES];
        let mut p = s.parts.pane_mut(PaneId::of(1)).ok_or("no %1")?;
        p.push(full).map_err(|e| e.to_string())?;
        let notice = |s: &mut Session| {
            let view = s.views.get_mut(&c);
            view.and_then(|v| v.notice.take()).map(|n| n.text)
        };
        s.input(c, b"\x1b[<0;3;2M");
        assert!(notice(&mut s).is_some_and(|n| n.contains("%1: input dropped (1)")));
        let typed = [
            b"x".as_slice(),
            b"y",
            b"\x1b[200~z\x1b[201~",
            b"\x1b[<0;3;2M",
            b"\x1b[I",
        ];
        for typed in typed {
            s.input(c, typed);
            let told = notice(&mut s);
            assert!(
                told.is_some_and(|n| n.contains("%1: input dropped (1)")),
                "{typed:?}"
            );
        }
        Ok(())
    }

    /// The first key a full queue refuses is told, though a run of drops
    /// before it, of replies, was told already.
    #[test]
    fn the_first_key_refused_is_told() -> Outcome {
        let (mut s, c) = session()?;
        let queries: Vec<u8> = std::iter::repeat_n(*b"\x1b[6n", crate::pane::REPLIES_MAX)
            .flatten()
            .collect();
        s.output(PaneId::of(1), &queries);
        let mut p = s.parts.pane_mut(PaneId::of(1)).ok_or("no %1")?;
        let room = p.input().room();
        p.push(vec![0; room]).map_err(|e| e.to_string())?;
        let notice = |s: &mut Session| {
            let view = s.views.get_mut(&c);
            view.and_then(|v| v.notice.take()).map(|n| n.text)
        };
        assert!(notice(&mut s).is_some_and(|n| n.contains("%1: replies to its program dropped")));
        s.input(c, b"x");
        assert!(notice(&mut s).is_some_and(|n| n.contains("%1: input dropped (1)")));
        Ok(())
    }

    /// A key or paste typed into a pane whose program exited is refused,
    /// as `send-keys` is, and told: never queued for no one to read.
    #[test]
    fn a_key_into_an_exited_program_is_refused_and_told() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "agent-opts 80 24 size str /bin/cat program run")?;
        run(&mut s, "str c1 client str %2 pane focus-pane")?;
        s.exited(PaneId::of(2), crate::process::Ended::Exited(3));
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
        run(&mut s, "str %1 pane horizontal split")?;
        run(&mut s, "str c1 client str %2 pane focus-pane")?;
        let right = rect_of(&s, c, 2).ok_or("no rect for %2")?;
        s.output(PaneId::of(2), b"\x1b[?1002h\x1b[?1006h");
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
    fn the_mouse_is_reported_only_while_no_screen_of_the_clients_is_open() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str %1 pane horizontal split")?;
        run(&mut s, "str c1 client str %2 pane focus-pane")?;
        s.output(PaneId::of(2), b"\x1b[?1003h\x1b[?1006h");
        assert_eq!(asked_mouse_level(&s, c), Some(1003));
        let right = rect_of(&s, c, 2).ok_or("no rect for %2")?;
        let press = format!("\x1b[<0;{};2M", right.x() + 2);
        // Copy mode, the column and a prompt have the keys: no reporting,
        // and a report in flight is dropped.
        for (open, close) in [
            (&b"\x02c"[..], &b"q"[..]),
            (b"\x02", b"\x1b"),
            (b"\x02e", b"\x1b"),
        ] {
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
        run(&mut s, "str c1 client str %1 pane focus-pane")?;
        assert_eq!(asked_mouse_level(&s, c), Some(0));
        s.input(c, b"\x1b[<0;2;2M");
        assert!(queued(&mut s, 1).is_empty() && queued(&mut s, 2).is_empty());
        // Zoomed, the pane fills the screen, and its cells are the screen's.
        run(&mut s, "str c1 client str %2 pane focus-pane")?;
        run(&mut s, "str c1 client zoom")?;
        s.input(c, b"\x1b[<0;2;2M");
        assert_eq!(queued(&mut s, 2), b"\x1b[<0;2;2M");
        Ok(())
    }

    /// A chooser about a pane that closes closes too, on the server and the
    /// client, saying why: its entries never act on another pane.
    #[test]
    fn a_list_whose_item_disappears_closes_and_never_retargets() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str %1 pane horizontal split")?;
        run(&mut s, "str c1 client str %2 pane focus-pane")?;
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
        assert_eq!(mode(&s, c), "confirm close workspace +1 new?");
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
        run(&mut s, "str c1 client str @2 tab show-tab")?;
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
