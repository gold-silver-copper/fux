//! fux's own screens: choosers, action menus, the command prompt, rename
//! prompts and confirmations, each belonging to the client that opened it,
//! and the command column and repeat bar a client shows of its own keys.
//! A client's keys reach them as typed actions (`crate::act`), made by its
//! mode tables (`crate::keymap`); a key reaches a pane only as literal
//! input (`send_key`).
use crate::act::{LineAct, ListAct};
use crate::command::{
    AnyRef, ClientAction, Command, Kind, MoveTo, Pick, Sibling, Subject, SwapWith, WsRef,
};
use crate::id::{ClientId, PaneId, WsId};
use crate::keys::Keystroke;
use crate::layout::Tree;
use crate::session::{Error, Outgoing, Place, Session, describe};
use crate::view::{Choice, Confirm, Item, Line, List, Mode, Prompt, PromptFor, View};

pub fn open_prompt(view: &mut View, purpose: PromptFor, title: String, text: String) -> String {
    view.mode = Mode::Prompt(Prompt {
        title,
        purpose,
        line: Line::new(text),
    });
    String::new()
}

pub fn open_confirm(session: &Session, view: &mut View, target: AnyRef<WsId>) -> String {
    let command = match target {
        AnyRef::Pane(p) => Command::KillPane { target: Some(p) },
        AnyRef::Tab(t) => Command::KillTab { target: Some(t) },
        AnyRef::Workspace(w) => Command::KillWorkspace {
            target: Some(WsRef::Id(w)),
        },
    };
    let (kind, name) = (target.kind().name(), session.name_of(target));
    let question = format!("close {kind} {} {name}?", describe(target));
    view.mode = Mode::Confirm(Confirm {
        question,
        command,
        about: target,
    });
    String::new()
}

fn item(label: &str, command: Command) -> Item {
    Item {
        label: label.to_owned(),
        command,
        current: false,
        subject: None,
    }
}

/// An action menu for a pane, tab or workspace: what has no default key.
/// What the menu is for, `about`, every item names.
pub fn open_menu(session: &Session, view: &mut View, about: AnyRef<WsId>) -> String {
    let kind = about.kind();
    let name = session.name_of(about);
    let title = format!("{} {} {name}", kind.name(), describe(about));
    let subject = Subject::from(about.map(WsRef::Id));
    let reorder = |toward| Command::Reorder {
        subject: subject.clone(),
        toward,
    };
    let rename = item("rename", ClientAction::RenamePrompt(subject.clone()).here());
    let mut items = vec![item(
        "close",
        ClientAction::ConfirmClose(subject.clone()).here(),
    )];
    match about {
        AnyRef::Pane(p) => items.extend([
            item(
                "terminate the running command",
                Command::Signal {
                    target: Some(p),
                    signal: fuxix::process::Signal::Term,
                    then: None,
                },
            ),
            item(
                "swap with…",
                ClientAction::ChoosePane { target: Some(p) }.here(),
            ),
            item(
                "move to tab…",
                ClientAction::ChooseTab {
                    moving: Some(Some(p)),
                }
                .here(),
            ),
            item(
                "move to a new tab",
                Command::MovePane {
                    target: Some(p),
                    to: MoveTo::NewTab,
                },
            ),
            item(
                "move to workspace…",
                ClientAction::ChooseWorkspace {
                    moving: Some(Some(p)),
                }
                .here(),
            ),
            item(
                "move to a new workspace",
                Command::MovePane {
                    target: Some(p),
                    to: MoveTo::NewWorkspace,
                },
            ),
        ]),
        AnyRef::Tab(tab) => {
            items.push(item(
                "new tab",
                Command::NewTab {
                    target: session.tab_workspace(tab).ok().map(WsRef::Id),
                    name: None,
                    cmd: Vec::new(),
                    size: None,
                    cwd: None,
                    env: Vec::new(),
                    ids: Vec::new(),
                    json: false,
                },
            ));
        }
        AnyRef::Workspace(_) => items.push(item(
            "new workspace",
            Command::NewWorkspace {
                name: None,
                cmd: Vec::new(),
                size: None,
                cwd: None,
                env: Vec::new(),
                ids: Vec::new(),
                json: false,
            },
        )),
    }
    items.extend([
        item("reorder previous", reorder(Sibling::Previous)),
        item("reorder next", reorder(Sibling::Next)),
    ]);
    let items = Choice::of(rename, items);
    open_list(view, title, items, Some(about))
}

/// Opens a list, the current item chosen.
fn open_list(
    view: &mut View,
    title: String,
    mut items: Choice<Item>,
    about: Option<AnyRef<WsId>>,
) -> String {
    let current = items.iter().position(|i| i.current);
    items.down(current.unwrap_or(0));
    view.mode = Mode::List(List {
        title,
        items,
        about,
    });
    String::new()
}

/// The names of the panes in `roots`, for a chooser row, shortened.
fn pane_names<'a>(session: &Session, roots: impl IntoIterator<Item = &'a Tree>) -> String {
    let mut names: Vec<String> = Vec::new();
    for root in roots {
        root.for_each_pane(&mut |id| {
            if let Some(p) = session.panes.get(&id) {
                names.push(format!("{} {}", p.id, p.label()));
            }
        });
    }
    if names.is_empty() {
        "empty".into()
    } else {
        names.join(", ")
    }
}

/// The tabs of the client's workspace. Enter selects one, or moves `moving`
/// there; `r` renames and `x` closes.
pub fn open_tab_chooser(
    session: &Session,
    view: &mut View,
    place: Place,
    moving: Option<PaneId>,
) -> Result<String, Error> {
    let ws = (session.workspace(place.ws)).ok_or(Error::NoWorkspace(place.ws))?;
    let items = (ws.tabs().iter())
        .map(|tab| {
            let (id, name) = (tab.id, &tab.name);
            Item {
                label: format!("{id} {name} — {}", pane_names(session, tab.root())),
                command: match moving {
                    Some(p) => Command::MovePane {
                        target: Some(p),
                        to: MoveTo::Tab(id),
                    },
                    None => ClientAction::SelectTab(Pick::Id(id)).here(),
                },
                current: id == place.tab,
                subject: Some(AnyRef::Tab(id)),
            }
        })
        .collect();
    let items = Choice::new(items).ok_or(Error::NoWorkspace(place.ws))?;
    Ok(open_chooser(view, "tab", items, moving))
}

/// Every workspace, with its tabs' panes.
pub fn open_workspace_chooser(
    session: &Session,
    view: &mut View,
    moving: Option<PaneId>,
) -> Result<String, Error> {
    let current = session.shown_workspace(view.id).map(|w| w.id);
    let items = session
        .workspaces
        .iter()
        .map(|ws| {
            let roots = ws.tabs().iter().filter_map(|t| t.root());
            Item {
                label: format!("{} {} — {}", ws.id, ws.name, pane_names(session, roots)),
                command: match moving {
                    Some(p) => Command::MovePane {
                        target: Some(p),
                        to: MoveTo::Workspace(WsRef::Id(ws.id)),
                    },
                    None => ClientAction::SelectWorkspace(Pick::Id(WsRef::Id(ws.id))).here(),
                },
                current: Some(ws.id) == current,
                subject: Some(AnyRef::Workspace(ws.id)),
            }
        })
        .collect();
    let items = Choice::new(items).ok_or(Error::NoWorkspaces)?;
    Ok(open_chooser(view, "workspace", items, moving))
}

/// A chooser of tabs or workspaces, `what`, to select one or to move a pane
/// to.
fn open_chooser(
    view: &mut View,
    what: &str,
    items: Choice<Item>,
    moving: Option<PaneId>,
) -> String {
    let title = match moving {
        Some(p) => format!("move {p} to {what}"),
        None => format!("{what}s"),
    };
    open_list(view, title, items, moving.map(AnyRef::Pane))
}

/// The other panes of `source`'s tab, to swap it with.
pub fn pane_chooser(session: &Session, source: PaneId) -> Result<List, Error> {
    let mut items: Vec<Item> = Vec::new();
    let root = session
        .locate(source)
        .and_then(|(_, tab)| session.root(tab));
    if let Some(root) = root {
        root.for_each_pane(&mut |id| {
            let Some(p) = session.panes.get(&id).filter(|_| id != source) else {
                return;
            };
            items.push(Item {
                label: format!("{} {}", p.id, p.label()),
                command: Command::SwapPane {
                    target: Some(source),
                    with: SwapWith::Pane(p.id),
                },
                current: false,
                subject: Some(AnyRef::Pane(p.id)),
            });
        });
    }
    let items = Choice::new(items).ok_or(Error::OnlyOne(Kind::Pane))?;
    let (title, about) = (format!("swap {source} with"), Some(AnyRef::Pane(source)));
    Ok(List {
        title,
        items,
        about,
    })
}

// ----------------------------------------------------------------- input

/// Runs an entry of a list, unless it cannot run now, in which case the
/// reason is shown and the list stays open.
fn run_entry(session: &mut Session, client: ClientId, command: &Command) {
    if let Some(reason) = session.unavailable(command, client) {
        return session.error_to(client, reason.to_string());
    }
    session.set_mode(client, Mode::Normal);
    session.run_for(client, command);
}

/// Rows a scrolled panel has for its entries on a screen of `rows`: the
/// rows above the bar, less `fixed` lines of its own (a title, a help line)
/// and the two lines that say how many entries lie above and below; at
/// least one.
pub fn panel_room(rows: u16, fixed: usize) -> usize {
    usize::from(rows.saturating_sub(1))
        .saturating_sub(fixed.saturating_add(2))
        .max(1)
}

/// The first of `len` entries a panel with `room` rows for them shows, so
/// that `selected` is in view: the window ends at the selection, or at the
/// last entry.
pub fn window_start(len: usize, selected: usize, room: usize) -> usize {
    selected
        .saturating_add(1)
        .saturating_sub(room)
        .min(len.saturating_sub(room))
}

/// Rows a chooser or menu shows its items in: it has a title and a help
/// line.
pub fn list_room(rows: u16) -> usize {
    panel_room(rows, 2)
}

/// Whether the command column has room for its heading, and the rows it
/// shows its entries in.
pub fn column_room(rows: u16) -> (bool, usize) {
    let heading = rows.saturating_sub(1) >= 4;
    (heading, panel_room(rows, usize::from(heading)))
}

/// Sends a key to the client's focused pane, encoded as its program asked.
pub fn send_key(session: &mut Session, client: ClientId, stroke: Keystroke) {
    let Some(pane) = session.focused(client) else {
        return;
    };
    session.typed(client);
    let Some(p) = session.panes.get_mut(&pane) else {
        return;
    };
    // Refused already, and said so: the key goes no further. A client
    // typing into a program that has stopped reading costs nothing per key.
    if p.input.refusing() {
        return;
    }
    let mode = p.screen().key_mode();
    if let Err(error) = p
        .input
        .push_with(|out| crate::encode::key_bytes(stroke, mode, out))
    {
        session.error_to(client, error.to_string());
    }
}

/// An action in a chooser or a menu.
pub fn list_act(session: &mut Session, client: ClientId, act: ListAct) {
    let Some(view) = session.views.get_mut(&client) else {
        return;
    };
    let page = list_room(view.rows);
    let Mode::List(list) = &mut view.mode else {
        return;
    };
    view.dirty = true;
    let items = &mut list.items;
    let run = match act {
        ListAct::Up => return items.up(1),
        ListAct::Down => return items.down(1),
        ListAct::PageUp => return items.up(page),
        ListAct::PageDown => return items.down(page),
        ListAct::First => return items.up(usize::MAX),
        ListAct::Last => return items.down(usize::MAX),
        ListAct::Cancel => return session.set_mode(client, Mode::Normal),
        ListAct::Choose => items.chosen().command.clone(),
        ListAct::Rename | ListAct::Close => {
            let Some(subject) = items
                .chosen()
                .subject
                .map(|s| Subject::from(s.map(WsRef::Id)))
            else {
                return;
            };
            match act {
                ListAct::Rename => ClientAction::RenamePrompt(subject).here(),
                ListAct::Up
                | ListAct::Down
                | ListAct::PageUp
                | ListAct::PageDown
                | ListAct::First
                | ListAct::Last
                | ListAct::Choose
                | ListAct::Cancel
                | ListAct::Close => ClientAction::ConfirmClose(subject).here(),
            }
        }
    };
    // An unavailable entry explains itself and the list stays open.
    run_entry(session, client, &run);
}

/// An action in a prompt: a one-line editor.
pub fn prompt_act(session: &mut Session, client: ClientId, act: LineAct) {
    let Some(view) = session.views.get_mut(&client) else {
        return;
    };
    let Mode::Prompt(prompt) = &mut view.mode else {
        return;
    };
    view.dirty = true;
    match act {
        LineAct::Cancel => view.mode = Mode::Normal,
        LineAct::Submit => {
            let prompt = prompt.clone();
            view.mode = Mode::Normal;
            submit(session, client, prompt);
        }
        // Text: one line of it, nothing that controls.
        LineAct::Insert(text) => {
            let line: String = (text.chars())
                .take_while(|c| *c != '\n' && *c != '\r')
                .filter(|c| !c.is_control())
                .collect();
            prompt.line.insert(&line);
        }
        act @ (LineAct::Backspace
        | LineAct::Delete
        | LineAct::Left
        | LineAct::Right
        | LineAct::Home
        | LineAct::End) => crate::copy::edit(&mut prompt.line, &act),
    }
}

/// Pasted text goes into a prompt, one line of it; nothing else takes pastes.
pub fn prompt_paste(session: &mut Session, client: ClientId, text: &str) {
    prompt_act(session, client, LineAct::Insert(text.to_owned()));
}

fn submit(session: &mut Session, client: ClientId, prompt: Prompt) {
    match prompt.purpose {
        // fuxth, which the client runs, as it runs its keys' programs.
        PromptFor::Command => {
            let code = prompt.line.text();
            if !code.trim().is_empty() {
                session.outbox.push(Outgoing::Run(client, code));
            }
        }
        PromptFor::Rename(target) => {
            // A name is the text as typed.
            let target = target.map(WsRef::Id);
            let name = prompt.line.text();
            let rename = Command::Rename { target, name };
            session.run_for(client, &rename);
        }
    }
}

/// A confirmation answered: yes runs it; no cancels.
pub fn confirm(session: &mut Session, client: ClientId, yes: bool) {
    let Some(view) = session.views.get_mut(&client) else {
        return;
    };
    let Mode::Confirm(confirm) = &view.mode else {
        return;
    };
    view.dirty = true;
    let command = confirm.command.clone();
    view.mode = Mode::Normal;
    if yes {
        session.run_for(client, &command);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::ClientId;
    use crate::session::testing::{output, output_as, requests, run};
    use crate::view::Mode;

    type Outcome = Result<(), String>;

    /// A session without processes, one client attached.
    fn session() -> Result<(Session, ClientId), String> {
        crate::session::testing::attached(30, 100)
    }

    fn mode(session: &Session, client: ClientId) -> String {
        match session.views.get(&client).map(|v| &v.mode) {
            Some(Mode::Normal) => "normal".into(),
            // Its keys after the prefix: `C-b g` is the layer `g`.
            Some(Mode::Column(column)) => match column.keys.split_once(' ') {
                None => format!("column {}", column.selected),
                Some((_, layer)) => format!("column {layer} {}", column.selected),
            },
            Some(Mode::Repeat(bar)) => format!("repeat {bar}"),
            Some(Mode::List(list)) => format!("list {} {}", list.title, list.items.index()),
            Some(Mode::Prompt(prompt)) => {
                format!("prompt {}|{}", prompt.line.before(), prompt.line.after())
            }
            Some(Mode::Confirm(confirm)) => format!("confirm {}", confirm.question),
            Some(Mode::Copy(_)) => "copy".into(),
            None => "gone".into(),
        }
    }

    fn notice(session: &Session, client: ClientId) -> String {
        session
            .views
            .get(&client)
            .and_then(|v| v.notice.clone())
            .map(|n| n.text)
            .unwrap_or_default()
    }

    /// A name typed into a rename prompt is the name, whatever it looks
    /// like.
    #[test]
    fn a_rename_prompt_takes_any_name() -> Outcome {
        let (mut session, client) = session()?;
        for (open, name, read) in [
            ("str c1 client str %1 pane rename-prompt", "-dev", "%1"),
            ("str c1 client str @1 tab rename-prompt", "--", "@1"),
            (
                "str c1 client str +1 workspace rename-prompt",
                "x -w --",
                "+1",
            ),
        ] {
            run(&mut session, open)?;
            // Backspace clears what the prompt starts with, the current name.
            session.input(client, &[0x7f; 16]);
            session.input(client, name.as_bytes());
            session.input(client, b"\r");
            assert_eq!(notice(&session, client), "", "{open}");
            let listed = output(&mut session, "ls")?;
            assert!(
                listed.contains(&format!("{read} {name}")),
                "{open}: {listed}"
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
        run(&mut s, "str c1 client choose-tab")?;
        s.input(c, b"\x1b[H");
        let shown = screen_text(&s, c)?;
        assert!(shown.contains("@1 main"), "{shown}");
        Ok(())
    }

    /// A prompt's text is at most 4096 bytes, typed as pasted: a character
    /// that would pass the limit is not typed.
    #[test]
    fn a_prompt_holds_at_most_4096_bytes_typed() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str c1 client str %1 pane rename-prompt")?;
        s.input(c, &[0x7f; 16]);
        let mut paste = b"\x1b[200~".to_vec();
        paste.extend(std::iter::repeat_n(b'a', 4095));
        paste.extend_from_slice(b"\x1b[201~");
        s.input(c, &paste);
        s.input(c, "\u{754c}".as_bytes());
        let len = match s.views.get(&c).map(|v| &v.mode) {
            Some(Mode::Prompt(prompt)) => prompt.line.clone().text().len(),
            _ => return Err("no prompt".into()),
        };
        assert_eq!(len, 4095, "the three bytes of the glyph would pass 4096");
        Ok(())
    }

    /// Like copy mode's, a chooser's and a confirmation's letter keys work
    /// in either case: Caps Lock changes nothing.
    #[test]
    fn list_and_confirm_keys_ignore_case() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, NEW_TAB)?;
        run(&mut s, "str c1 client choose-tab")?;
        s.input(c, b"\x1b[H");
        s.input(c, b"J");
        assert!(mode(&s, c).ends_with(" 1"), "J moves down: {}", mode(&s, c));
        s.input(c, b"K");
        assert!(mode(&s, c).ends_with(" 0"), "K moves up: {}", mode(&s, c));
        s.input(c, b"Q");
        assert_eq!(mode(&s, c), "normal", "Q closes");
        run(&mut s, "str c1 client str @2 tab confirm-close")?;
        s.input(c, b"Q");
        assert_eq!(mode(&s, c), "normal", "Q cancels");
        run(&mut s, "str c1 client str @2 tab confirm-close")?;
        s.input(c, b"Y");
        let gone = crate::command::parse_tab("@2").is_ok_and(|t| s.tab(t).is_none());
        assert!(gone, "Y confirms");
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
        let downs: Vec<u8> = std::iter::repeat_n(&b"\x1b[B"[..], 30)
            .flatten()
            .copied()
            .collect();
        s.input(c, &downs);
        assert_eq!(mode(&s, c).split(' ').next_back(), Some("30"));
        let title = match s.views.get(&c).map(|v| &v.mode) {
            Some(Mode::List(list)) => list.title.clone(),
            _ => return Err("a list".into()),
        };
        let text = screen_text(&s, c)?;
        for shown in [title.as_str(), "▲", "▼", "Enter selects"] {
            assert!(text.contains(shown), "{shown:?} in\n{text}");
        }
        Ok(())
    }

    /// A tab in the first workspace.
    const NEW_TAB: &str = "opts str +1 workspace in new-tab";

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

    fn pane_width(s: &Session, pane: u32) -> u16 {
        s.panes
            .get(&crate::id::PaneId::of(pane))
            .map_or(0, |p| p.screen().size().cols())
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
        s.panes
            .get_mut(&crate::id::PaneId::of(pane))
            .map(|p| p.input.drain_all())
            .unwrap_or_default()
    }

    #[test]
    fn a_layer_runs_its_binding_on_the_next_key() -> Outcome {
        let (mut s, c) = session()?;
        with_layers(&mut s)?;
        let at = (s.test_keymap.column(&[], 0).ok_or("a column")?.entries)
            .iter()
            .position(|e| e.key == "g")
            .ok_or("the layer is listed")?;
        s.input(c, b"\x02g");
        assert_eq!(mode(&s, c), "column g 0");
        // The column shows the keys so far and the layer's title, and the
        // bar what has been typed.
        let text = screen_text(&s, c)?;
        assert!(text.contains("C-b g: Tabs"), "{text}");
        assert!(text.contains("C-b g …"), "{text}");
        assert!(text.contains("new tab"), "{text}");
        s.input(c, b"n");
        assert_eq!(mode(&s, c), "normal");
        assert_eq!(tabs(&s), Some(2));
        // Enter on the layer's entry opens it too.
        s.input(c, b"\x02");
        s.input(
            c,
            &std::iter::repeat_n(&b"\x1b[B"[..], at)
                .flatten()
                .copied()
                .collect::<Vec<u8>>(),
        );
        let text = screen_text(&s, c)?;
        assert!(text.contains("g  Tabs…"), "{text}");
        s.input(c, b"\r");
        assert_eq!(mode(&s, c), "column g 0");
        Ok(())
    }

    #[test]
    fn an_unbound_key_keeps_a_layer_open_and_the_prefix_sends_itself() -> Outcome {
        let (mut s, c) = session()?;
        with_layers(&mut s)?;
        s.input(c, b"\x02gf");
        assert_eq!(mode(&s, c), "column g 0");
        assert_eq!(notice(&s, c), "C-b g f is not bound");
        let _ = queued(&mut s, 1);
        s.input(c, b"\x02");
        assert_eq!(mode(&s, c), "normal");
        assert_eq!(queued(&mut s, 1), b"\x02");
        Ok(())
    }

    #[test]
    fn a_repeating_binding_keeps_its_layer_until_enter_or_esc() -> Outcome {
        let (mut s, c) = session()?;
        with_layers(&mut s)?;
        run(&mut s, "str %1 pane opts horizontal split")?;
        run(&mut s, "str c1 client str %1 pane focus-pane")?;
        let start = pane_width(&s, 1);
        s.input(c, b"\x02y");
        assert_eq!(mode(&s, c), "column y 0");
        s.input(c, b"l");
        // The bar names the mode and its keys.
        assert_eq!(mode(&s, c), "repeat GROW  l h · Esc");
        let grid = crate::render::compose(&s, c).ok_or("a screen")?;
        let bar = grid.row_text(grid.rows.saturating_sub(1));
        assert!(bar.ends_with("GROW  l h · Esc"), "{bar}");
        // No prefix: the mode's keys run again and again.
        s.input(c, b"ll");
        assert_eq!(start.checked_add(3), Some(pane_width(&s, 1)));
        s.input(c, b"h");
        assert_eq!(start.checked_add(2), Some(pane_width(&s, 1)));
        s.input(c, b"\r");
        assert_eq!(mode(&s, c), "normal");
        // Esc leaves too, once its deadline passes.
        s.input(c, b"\x02yl\x1b");
        std::thread::sleep(crate::decode::ESCAPE_DELAY);
        s.escape(c);
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

    /// A session with room for every command to act: two workspaces, the
    /// first with two tabs, its first with three panes, 1 | (2 / 3), the
    /// client on %2.
    fn busy() -> Result<(Session, ClientId), String> {
        let (mut s, c) = session()?;
        run(&mut s, "str %1 pane opts horizontal split")?;
        run(&mut s, "str %2 pane opts vertical split")?;
        run(&mut s, NEW_TAB)?;
        run(&mut s, "opts new-workspace")?;
        run(&mut s, "str c1 client str %2 pane focus-pane")?;
        Ok((s, c))
    }

    /// What a user can tell apart: every workspace, tab, pane and client,
    /// the client's mode, and its notice.
    fn state(s: &mut Session, c: ClientId) -> Result<String, String> {
        let ls = output(s, "ls")?;
        Ok(format!("{ls}{}\n{}", mode(s, c), notice(s, c)))
    }

    /// The prefix, then `keys`.
    fn prefixed(keys: &str) -> Vec<u8> {
        std::iter::once(0x02).chain(keys.bytes()).collect()
    }

    fn escape(s: &mut Session, c: ClientId) {
        s.input(c, b"\x1b");
        std::thread::sleep(crate::decode::ESCAPE_DELAY);
        s.escape(c);
    }

    /// Typing a default binding's keys does what running its program as the
    /// client does, and the program runs; a repeating one also enters its
    /// mode.
    #[test]
    fn every_default_binding_runs_its_program() -> Outcome {
        let keymap = crate::keymap::Keymap::default();
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
                ran.error_to(d, error);
            }
            assert_eq!(state(&mut typed, c)?, state(&mut ran, d)?, "{keys}");
        }
        // Refused as the session stands: no pane that way, no copy to
        // paste, nothing before the first tab or workspace.
        assert_eq!(refused, ["k", "l", "p", "mk", "ml", "tmh", "wmh"]);
        Ok(())
    }

    /// Menus, choosers and confirmations build their commands, typed,
    /// with every target given: each is what the words for it ask.
    #[test]
    fn listed_commands_are_what_their_words_ask() -> Outcome {
        let (mut s, c) = busy()?;
        let listed = |s: &Session| match s.views.get(&c).map(|v| &v.mode) {
            Some(Mode::List(list)) => list.items.iter().map(|i| i.command.clone()).collect(),
            Some(Mode::Confirm(confirm)) => vec![confirm.command.clone()],
            _ => Vec::new(),
        };
        for (open, programs) in [
            (
                "me str %2 pane menu",
                &[
                    "me str %2 pane rename-prompt",
                    "me str %2 pane confirm-close",
                    "str %2 pane str TERM signal",
                    "me str %2 pane choose-pane",
                    "me str %2 pane choose-tab-for",
                    "str %2 pane str new-tab move-to",
                    "me str %2 pane choose-workspace-for",
                    "str %2 pane str new-workspace move-to",
                    "str %2 pane str previous reorder",
                    "str %2 pane str next reorder",
                ][..],
            ),
            (
                "me str @1 tab menu",
                &[
                    "me str @1 tab rename-prompt",
                    "me str @1 tab confirm-close",
                    "opts str +1 workspace in new-tab",
                    "str @1 tab str previous reorder",
                    "str @1 tab str next reorder",
                ],
            ),
            (
                "me str main workspace menu",
                &[
                    "me str +1 workspace rename-prompt",
                    "me str +1 workspace confirm-close",
                    "opts new-workspace",
                    "str +1 workspace str previous reorder",
                    "str +1 workspace str next reorder",
                ],
            ),
            (
                "me choose-tab",
                &["me str @1 tab show-tab", "me str @2 tab show-tab"],
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
                "me str main workspace confirm-close",
                &["str +1 workspace kill"],
            ),
        ] {
            output_as(&mut s, Some(c), open)?;
            let mut expected = Vec::new();
            for program in programs {
                expected.extend(requests(&mut s, c, program)?);
            }
            assert_eq!(listed(&s), expected, "{open}");
        }
        // A chooser's `r` and `x` act on the selected item.
        output_as(&mut s, Some(c), "me choose-tab")?;
        s.input(c, b"r");
        assert_eq!(mode(&s, c), "prompt main|");
        s.input(c, b"\r");
        output_as(&mut s, Some(c), "me choose-tab")?;
        s.input(c, b"x");
        assert_eq!(mode(&s, c), "confirm close tab @1 main?");
        Ok(())
    }

    /// How many tabs the first workspace has.
    fn tabs(s: &Session) -> Option<usize> {
        s.workspaces.first().map(|w| w.tabs().len())
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
        let pane = s.focused(c).map_or(0, PaneId::number);
        let _ = queued(&mut s, pane);
        s.input(c, &[0x02, 0x02]);
        assert_eq!(queued(&mut s, pane), b"\x02");
        run(&mut s, "str C-b str 'opts new-tab drop' bind")?;
        s.input(c, &[0x02, 0x02]);
        assert_eq!(tabs(&s), Some(3));
        let pane = s.focused(c).map_or(0, PaneId::number);
        assert!(queued(&mut s, pane).is_empty());
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

    #[test]
    fn a_repeat_mode_may_bind_enter() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str %1 pane opts horizontal split")?;
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
        let pane = s.focused(c).map_or(0, PaneId::number);
        assert!(queued(&mut s, pane).is_empty());
        // A chord bound to nothing reaches the pane as it was typed.
        s.input(c, b"\x1bl");
        assert_eq!(queued(&mut s, pane), b"\x1bl");
        // In copy mode and in the column the key is theirs, not the binding's.
        run(&mut s, "str c1 client copy-mode")?;
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
        s.input(c, &prefixed(""));
        for _ in 1..entries.len() {
            s.input(c, b"\x1b[B");
        }
        s.input(c, b"\r");
        assert_eq!(tabs(&s), Some(3));
        Ok(())
    }

    /// Pane `pane`'s rect on client `c`'s screen.
    fn rect_of(s: &Session, c: ClientId, pane: u32) -> Option<crate::layout::Rect> {
        let mut placement = crate::layout::Placement::default();
        s.placement_into(s.views.get(&c)?, &mut placement);
        placement.rect(crate::id::PaneId::of(pane))
    }

    /// The mouse tracking client `c`'s terminal is asked for.
    fn asked_mouse_level(s: &Session, c: ClientId) -> Option<u16> {
        crate::render::compose(s, c).map(|g| g.mouse)
    }

    #[test]
    fn a_mouse_report_reaches_the_focused_pane_in_its_cells() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str %1 pane opts horizontal split")?;
        run(&mut s, "str c1 client str %2 pane focus-pane")?;
        let right = rect_of(&s, c, 2).ok_or("no rect for %2")?;
        assert!(right.x() > 1 && right.y() == 0, "{right:?}");
        // Nothing asked: no reporting, and a report is dropped.
        assert_eq!(asked_mouse_level(&s, c), Some(0));
        let press = |col: u16, row: u16| format!("\x1b[<0;{};{}M", col + 1, row + 1);
        s.input(c, press(right.x() + 3, 2).as_bytes());
        assert!(queued(&mut s, 2).is_empty());
        s.output(crate::id::PaneId::of(2), b"\x1b[?1000h\x1b[?1006h");
        assert_eq!(asked_mouse_level(&s, c), Some(1000));
        // Moved to the pane's cells.
        s.input(c, press(right.x() + 3, 2).as_bytes());
        assert_eq!(queued(&mut s, 2), b"\x1b[<0;4;3M");
        // On the other pane, or the border between them: dropped.
        s.input(c, press(1, 2).as_bytes());
        s.input(c, press(right.x() - 1, 2).as_bytes());
        assert!(queued(&mut s, 2).is_empty() && queued(&mut s, 1).is_empty());
        // In the encoding the program asked for: here the default.
        s.output(crate::id::PaneId::of(2), b"\x1b[?1006l");
        s.input(c, press(right.x(), 0).as_bytes());
        assert_eq!(queued(&mut s, 2), [0x1b, b'[', b'M', 32, 33, 33]);
        // X10's presses come from asking for 1000, its releases dropped.
        s.output(crate::id::PaneId::of(2), b"\x1b[?9h\x1b[?1006h");
        assert_eq!(asked_mouse_level(&s, c), Some(1000));
        s.input(
            c,
            format!("{}\x1b[<0;{};1m", press(right.x(), 0), right.x() + 1).as_bytes(),
        );
        assert_eq!(queued(&mut s, 2), b"\x1b[<0;1;1M");
        // A pane below another: moved down too.
        run(&mut s, "str %2 pane opts vertical split")?;
        run(&mut s, "str c1 client str %3 pane focus-pane")?;
        let below = rect_of(&s, c, 3).ok_or("no rect for %3")?;
        assert!(below.y() > 1, "{below:?}");
        s.output(crate::id::PaneId::of(3), b"\x1b[?1000h\x1b[?1006h");
        s.input(c, press(below.x() + 1, below.y() + 2).as_bytes());
        assert_eq!(queued(&mut s, 3), b"\x1b[<0;2;3M");
        Ok(())
    }

    #[test]
    fn a_drag_out_of_the_pane_is_kept_to_its_edge() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str %1 pane opts horizontal split")?;
        run(&mut s, "str c1 client str %2 pane focus-pane")?;
        let right = rect_of(&s, c, 2).ok_or("no rect for %2")?;
        s.output(crate::id::PaneId::of(2), b"\x1b[?1002h\x1b[?1006h");
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
    fn the_mouse_is_reported_only_while_the_focused_program_has_the_keys() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str %1 pane opts horizontal split")?;
        run(&mut s, "str c1 client str %2 pane focus-pane")?;
        s.output(crate::id::PaneId::of(2), b"\x1b[?1003h\x1b[?1006h");
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
            std::thread::sleep(crate::decode::ESCAPE_DELAY);
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
        run(&mut s, "str %1 pane opts horizontal split")?;
        run(&mut s, "str %2 pane opts horizontal split")?;
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
        run(&mut s, "opts str +1 workspace in str two name new-tab")?;
        run(&mut s, "opts str +1 workspace in str three name new-tab")?;
        run(&mut s, "str c1 client str @1 tab show-tab")?;
        s.input(c, &prefixed("tmll"));
        assert_eq!(mode(&s, c), "repeat REORDER  h l · Esc");
        escape(&mut s, c);
        let order: Vec<String> = s
            .workspaces
            .first()
            .map(|w| w.tabs().iter().map(|t| t.id.to_string()).collect())
            .unwrap_or_default();
        assert_eq!(order, ["@2", "@3", "@1"]);
        Ok(())
    }

    #[test]
    fn a_list_whose_item_disappears_closes_and_never_retargets() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str %1 pane opts horizontal split")?;
        run(&mut s, "str c1 client str %2 pane focus-pane")?;
        s.input(c, b"\x02a");
        run(&mut s, "str %2 pane kill")?;
        assert_eq!(mode(&s, c), "normal");
        assert!(notice(&s, c).contains("%2 is gone"), "{}", notice(&s, c));
        Ok(())
    }

    /// A workspace named by its name is held by its ID: renamed while a
    /// prompt or a confirmation for it is open, it is the same workspace,
    /// and they stay open on it.
    #[test]
    fn an_overlay_holds_a_workspace_named_by_name_through_a_rename() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "str c1 client str main workspace rename-prompt")?;
        run(&mut s, "str main workspace str other rename")?;
        assert_eq!(mode(&s, c), "prompt main|", "{}", notice(&s, c));
        s.input(c, b"\x7f\x7f\x7f\x7fnew\r");
        assert_eq!(s.workspaces.first().map(|w| w.name.as_str()), Some("new"));
        run(&mut s, "str c1 client str new workspace confirm-close")?;
        run(&mut s, "str new workspace str again rename")?;
        assert_eq!(mode(&s, c), "confirm close workspace +1 new?");
        Ok(())
    }

    #[test]
    fn choosers_mark_the_current_item_and_start_on_it() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "opts str +1 workspace in str second name new-tab")?;
        run(&mut s, "str c1 client str @2 tab show-tab")?;
        s.input(c, b"\x02tg");
        let Some(Mode::List(list)) = s.views.get(&c).map(|v| &v.mode) else {
            return Err(mode(&s, c));
        };
        assert_eq!(list.items.index(), 1);
        let current: Vec<bool> = list.items.iter().map(|i| i.current).collect();
        assert_eq!(current, [false, true]);
        assert!(list.items.iter().all(|i| i.label.contains("— %")));
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
        s.input(c, b"\x1b[200~one\ntwo\x1b[201~");
        assert_eq!(mode(&s, c), "prompt one|");
        escape(&mut s, c);
        assert_eq!(mode(&s, c), "normal");
        Ok(())
    }
}
