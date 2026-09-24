//! The keyboard overlays: the command column, choosers, action menus, the
//! `:` prompt, rename prompts and confirmations. Each belongs to the client
//! that opened it.
use crate::command::{AnyRef, ClientId, Kind, TabId, WsRef};
use crate::keys::{Direction, Key, KeyPress};
use crate::layout::PaneId;
use crate::session::{Ctx, Session, describe};
use crate::view::{Confirm, Item, List, Mode, Prompt, PromptFor};

/// One row of the command column: a group heading or a binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColumnRow {
    Heading(String),
    Binding {
        key: String,
        label: String,
        argv: Vec<String>,
    },
}

/// The command column's rows: every binding, grouped, groups in their order,
/// custom groups after them and `Other` last.
pub fn column_rows(session: &Session) -> Vec<ColumnRow> {
    let bindings = &session.config.bindings;
    let mut groups: Vec<String> = crate::config::GROUPS
        .iter()
        .map(|g| (*g).to_owned())
        .collect();
    for binding in bindings {
        let group = binding.group();
        if !groups.contains(&group) && group != "Other" {
            groups.push(group);
        }
    }
    groups.push("Other".into());
    let mut rows = Vec::new();
    for group in groups {
        let members: Vec<_> = bindings.iter().filter(|b| b.group() == group).collect();
        if members.is_empty() {
            continue;
        }
        rows.push(ColumnRow::Heading(group));
        for binding in members {
            rows.push(ColumnRow::Binding {
                key: binding.key.to_string(),
                label: crate::command::label(&binding.command),
                argv: binding.command.clone(),
            });
        }
    }
    rows
}

/// How many bindings the column can select among.
fn column_len(session: &Session) -> usize {
    session.config.bindings.len()
}

/// The command line of the column's `selected` binding.
pub fn column_selected(session: &Session, selected: usize) -> Option<Vec<String>> {
    column_rows(session)
        .into_iter()
        .filter_map(|row| match row {
            ColumnRow::Binding { argv, .. } => Some(argv),
            ColumnRow::Heading(_) => None,
        })
        .nth(selected)
}

pub fn open_prompt(
    session: &mut Session,
    client: ClientId,
    purpose: PromptFor,
    title: String,
    text: String,
) -> Result<String, String> {
    let view = session.views.get_mut(&client).ok_or("no such client")?;
    let cursor = text.chars().count();
    view.mode = Mode::Prompt(Prompt {
        title,
        purpose,
        text,
        cursor,
    });
    Ok(String::new())
}

pub fn open_confirm(
    session: &mut Session,
    client: ClientId,
    target: AnyRef,
) -> Result<String, String> {
    let (kind, command) = match &target {
        AnyRef::Pane(_) => ("pane", "kill-pane"),
        AnyRef::Tab(_) => ("tab", "kill-tab"),
        AnyRef::Workspace(_) => ("workspace", "kill-workspace"),
    };
    let name = session.name_of(&target);
    let id = match &target {
        AnyRef::Workspace(r) => session.resolve_ws(r)?.to_string(),
        other => describe(other),
    };
    let question = format!("close {kind} {id} {name}?");
    let view = session.views.get_mut(&client).ok_or("no such client")?;
    view.mode = Mode::Confirm(Confirm {
        question,
        argv: vec![command.into(), "-t".into(), id],
        about: target,
    });
    Ok(String::new())
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

fn item(label: &str, words: Vec<String>) -> Item {
    Item {
        label: label.to_owned(),
        argv: words,
        current: false,
        subject: None,
    }
}

/// An action menu for a pane, tab or workspace: what has no default key.
pub fn open_menu(
    session: &mut Session,
    client: ClientId,
    target: AnyRef,
) -> Result<String, String> {
    let (title, items) = match &target {
        AnyRef::Pane(p) => {
            let t = p.to_string();
            let with = |words: &[&str]| {
                let mut v = argv(words);
                v.push("-t".into());
                v.push(t.clone());
                v
            };
            let name = session.name_of(&target);
            (
                format!("pane {p} {name}"),
                vec![
                    item("rename", with(&["rename-prompt", "pane"])),
                    item("close", with(&["confirm-close", "pane"])),
                    item("terminate the running command", with(&["terminate"])),
                    item("swap with…", with(&["choose-pane"])),
                    item("move to tab…", with(&["choose-tab"])),
                    item("move to a new tab", with(&["move-pane", "--to", "new-tab"])),
                    item("move to workspace…", with(&["choose-workspace"])),
                    item(
                        "move to a new workspace",
                        with(&["move-pane", "--to", "new-workspace"]),
                    ),
                    item("reorder previous", with(&["reorder", "pane", "--previous"])),
                    item("reorder next", with(&["reorder", "pane", "--next"])),
                ],
            )
        }
        AnyRef::Tab(tab) => {
            let t = tab.to_string();
            let with = |words: &[&str]| {
                let mut v = argv(words);
                v.push("-t".into());
                v.push(t.clone());
                v
            };
            let name = session.name_of(&target);
            let ws = session
                .find_tab(*tab)
                .and_then(|(w, _)| session.workspaces.get(w))
                .map(|w| w.id.to_string())
                .unwrap_or_default();
            (
                format!("tab {tab} {name}"),
                vec![
                    item("rename", with(&["rename-prompt", "tab"])),
                    item("close", with(&["confirm-close", "tab"])),
                    item("new tab", vec!["new-tab".into(), "-t".into(), ws]),
                    item("reorder previous", with(&["reorder", "tab", "--previous"])),
                    item("reorder next", with(&["reorder", "tab", "--next"])),
                ],
            )
        }
        AnyRef::Workspace(r) => {
            let id = session.resolve_ws(r)?;
            let t = id.to_string();
            let with = |words: &[&str]| {
                let mut v = argv(words);
                v.push("-t".into());
                v.push(t.clone());
                v
            };
            let name = session.name_of(&target);
            (
                format!("workspace {id} {name}"),
                vec![
                    item("rename", with(&["rename-prompt", "workspace"])),
                    item("close", with(&["confirm-close", "workspace"])),
                    item("new workspace", argv(&["new-workspace"])),
                    item(
                        "reorder previous",
                        with(&["reorder", "workspace", "--previous"]),
                    ),
                    item("reorder next", with(&["reorder", "workspace", "--next"])),
                ],
            )
        }
    };
    let about = match target {
        AnyRef::Workspace(r) => AnyRef::Workspace(WsRef::Id(session.resolve_ws(&r)?)),
        other => other,
    };
    open_list(session, client, title, items, false, Some(about))
}

fn open_list(
    session: &mut Session,
    client: ClientId,
    title: String,
    items: Vec<Item>,
    chooser: bool,
    about: Option<AnyRef>,
) -> Result<String, String> {
    let selected = items.iter().position(|i| i.current).unwrap_or(0);
    let view = session.views.get_mut(&client).ok_or("no such client")?;
    view.mode = Mode::List(List {
        title,
        items,
        selected,
        chooser,
        about,
    });
    Ok(String::new())
}

/// Panes' names for a chooser row, shortened.
fn pane_names(session: &Session, panes: &[PaneId]) -> String {
    let names: Vec<String> = panes
        .iter()
        .filter_map(|p| session.panes.get(p))
        .map(|p| format!("{} {}", p.id, p.label()))
        .collect();
    if names.is_empty() {
        "empty".into()
    } else {
        names.join(", ")
    }
}

/// The tabs of the client's workspace. Enter selects one, or moves `moving`
/// there; `r` renames and `x` closes.
pub fn open_tab_chooser(
    session: &mut Session,
    client: ClientId,
    moving: Option<PaneId>,
) -> Result<String, String> {
    let view = session.views.get(&client).ok_or("no such client")?;
    let current = view.tab();
    let ws = session.workspace(view.workspace).ok_or("no workspace")?;
    let tabs: Vec<(TabId, String)> = ws.tabs.iter().map(|t| (t.id, t.name.clone())).collect();
    let items = tabs
        .into_iter()
        .map(|(id, name)| {
            let panes = session.tab_panes(id);
            Item {
                label: format!("{id} {name} — {}", pane_names(session, &panes)),
                argv: match moving {
                    Some(p) => vec![
                        "move-pane".into(),
                        "-t".into(),
                        p.to_string(),
                        "--to".into(),
                        id.to_string(),
                    ],
                    None => vec!["select-tab".into(), "-t".into(), id.to_string()],
                },
                current: Some(id) == current,
                subject: Some(AnyRef::Tab(id)),
            }
        })
        .collect();
    let title = match moving {
        Some(p) => format!("move {p} to tab"),
        None => "tabs".into(),
    };
    open_list(
        session,
        client,
        title,
        items,
        true,
        moving.map(AnyRef::Pane),
    )
}

/// Every workspace, with its tabs' panes.
pub fn open_workspace_chooser(
    session: &mut Session,
    client: ClientId,
    moving: Option<PaneId>,
) -> Result<String, String> {
    let current = session
        .views
        .get(&client)
        .ok_or("no such client")?
        .workspace;
    let items = session
        .workspaces
        .iter()
        .map(|ws| {
            let panes: Vec<PaneId> = ws
                .tabs
                .iter()
                .flat_map(|t| session.tab_panes(t.id))
                .collect();
            Item {
                label: format!("{} {} — {}", ws.id, ws.name, pane_names(session, &panes)),
                argv: match moving {
                    Some(p) => vec![
                        "move-pane".into(),
                        "-t".into(),
                        p.to_string(),
                        "--to".into(),
                        ws.id.to_string(),
                    ],
                    None => vec!["select-workspace".into(), "-t".into(), ws.id.to_string()],
                },
                current: ws.id == current,
                subject: Some(AnyRef::Workspace(WsRef::Id(ws.id))),
            }
        })
        .collect();
    let title = match moving {
        Some(p) => format!("move {p} to workspace"),
        None => "workspaces".into(),
    };
    open_list(
        session,
        client,
        title,
        items,
        true,
        moving.map(AnyRef::Pane),
    )
}

/// The other panes of the client's tab, to swap `source` with.
pub fn open_pane_chooser(
    session: &mut Session,
    client: ClientId,
    source: PaneId,
) -> Result<String, String> {
    let (_, tab) = session.locate(source).ok_or("the pane is in no tab")?;
    let items: Vec<Item> = session
        .tab_panes(tab)
        .into_iter()
        .filter(|p| *p != source)
        .filter_map(|p| session.panes.get(&p))
        .map(|p| Item {
            label: format!("{} {}", p.id, p.label()),
            argv: vec![
                "swap-pane".into(),
                "-t".into(),
                source.to_string(),
                p.id.to_string(),
            ],
            current: false,
            subject: Some(AnyRef::Pane(p.id)),
        })
        .collect();
    if items.is_empty() {
        return Err("only one pane".into());
    }
    open_list(
        session,
        client,
        format!("swap {source} with"),
        items,
        true,
        Some(AnyRef::Pane(source)),
    )
}

// ----------------------------------------------------------------- input

/// Runs a command line for a client: output and errors become its notice.
pub fn run_for(session: &mut Session, client: ClientId, argv: &[String]) {
    let outcome = session.run(argv, &Ctx::client(client));
    if let Some(view) = session.views.get_mut(&client) {
        if outcome.status != 0 {
            view.error(outcome.stderr.lines().next().unwrap_or("failed").to_owned());
        } else if let Some(line) = outcome.stdout.lines().find(|l| !l.trim().is_empty())
            && !matches!(
                argv.first().map(String::as_str),
                Some("split" | "new-tab" | "new-workspace" | "move-pane")
            )
        {
            let more = outcome
                .stdout
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count()
                > 1;
            view.info(if more {
                format!("{line} …")
            } else {
                line.to_owned()
            });
        }
    }
}

/// Runs an entry of a list or the column, unless it cannot run now, in
/// which case the reason is shown and nothing happens.
fn run_entry(session: &mut Session, client: ClientId, argv: &[String]) {
    if let Some(reason) = session.unavailable(argv, &Ctx::client(client)) {
        if let Some(view) = session.views.get_mut(&client) {
            view.error(reason);
        }
        return;
    }
    run_for(session, client, argv);
}

fn plain(press: KeyPress) -> Option<Key> {
    (!press.mods.ctrl && !press.mods.alt).then_some(press.key)
}

/// Rows the list shows at once on a screen of `rows`.
pub fn list_capacity(rows: u16) -> usize {
    usize::from(rows.saturating_sub(4)).max(1)
}

/// A key while the command column is open.
pub fn column_key(session: &mut Session, client: ClientId, press: KeyPress) {
    let len = column_len(session);
    let prefix = session.config.prefix;
    let Some(view) = session.views.get_mut(&client) else {
        return;
    };
    let Mode::Column { selected } = view.mode else {
        return;
    };
    let page = list_capacity(view.rows);
    let last = len.saturating_sub(1);
    // The column navigates with unmodified keys only: modified arrows are
    // bindings (S-Left moves a pane, C-Left resizes, M-Left focuses).
    let unmodified = press.mods.is_empty().then_some(press.key);
    let new = match unmodified {
        _ if press == prefix => {
            // The prefix twice sends it to the pane.
            view.mode = Mode::Normal;
            send_key(session, client, prefix);
            return;
        }
        Some(Key::Arrow(Direction::Up)) | Some(Key::Char('k')) => selected.saturating_sub(1),
        Some(Key::Arrow(Direction::Down)) | Some(Key::Char('j')) => (selected + 1).min(last),
        Some(Key::PageUp) => selected.saturating_sub(page),
        Some(Key::PageDown) => (selected + page).min(last),
        Some(Key::Home) => 0,
        Some(Key::End) => last,
        Some(Key::Escape) => {
            view.mode = Mode::Normal;
            return;
        }
        Some(Key::Enter) => {
            view.mode = Mode::Normal;
            if let Some(argv) = column_selected(session, selected) {
                run_entry(session, client, &argv);
            }
            return;
        }
        _ => {
            let bound = session
                .config
                .bindings
                .iter()
                .find(|b| b.key == press)
                .map(|b| b.command.clone());
            match bound {
                Some(argv) => {
                    if let Some(view) = session.views.get_mut(&client) {
                        view.mode = Mode::Normal;
                    }
                    run_entry(session, client, &argv);
                }
                // An unbound key leaves the column open, saying so.
                None => {
                    if let Some(view) = session.views.get_mut(&client) {
                        view.error(format!("{prefix} {press} is not bound"));
                    }
                }
            }
            return;
        }
    };
    if let Some(view) = session.views.get_mut(&client) {
        view.mode = Mode::Column { selected: new };
        view.dirty = true;
    }
}

/// Sends a key to the client's focused pane.
pub fn send_key(session: &mut Session, client: ClientId, press: KeyPress) {
    let Some(pane) = session.views.get(&client).and_then(|v| v.focus()) else {
        return;
    };
    let Some(p) = session.panes.get_mut(&pane) else {
        return;
    };
    let bytes = crate::encode::key_bytes(press, p.screen().application_cursor());
    if let Err(error) = p.input.push(bytes)
        && let Some(view) = session.views.get_mut(&client)
    {
        view.error(error);
    }
}

/// A key in a chooser or a menu.
pub fn list_key(session: &mut Session, client: ClientId, press: KeyPress) {
    let Some(view) = session.views.get_mut(&client) else {
        return;
    };
    let rows = view.rows;
    let Mode::List(list) = &mut view.mode else {
        return;
    };
    let last = list.items.len().saturating_sub(1);
    let page = list_capacity(rows);
    let mut run: Option<Vec<String>> = None;
    let mut close = false;
    match plain(press) {
        Some(Key::Arrow(Direction::Up)) | Some(Key::Char('k')) => {
            list.selected = list.selected.saturating_sub(1)
        }
        Some(Key::Arrow(Direction::Down)) | Some(Key::Char('j')) => {
            list.selected = (list.selected + 1).min(last)
        }
        Some(Key::PageUp) => list.selected = list.selected.saturating_sub(page),
        Some(Key::PageDown) => list.selected = (list.selected + page).min(last),
        Some(Key::Home) => list.selected = 0,
        Some(Key::End) => list.selected = last,
        Some(Key::Escape) | Some(Key::Char('q')) => close = true,
        Some(Key::Enter) => {
            run = list.items.get(list.selected).map(|i| i.argv.clone());
            close = run.is_some();
        }
        Some(Key::Char('r')) if list.chooser => {
            if let Some(subject) = list
                .items
                .get(list.selected)
                .and_then(|i| i.subject.clone())
            {
                run = Some(vec![
                    "rename-prompt".into(),
                    "-t".into(),
                    describe_id(session_ws(&subject)),
                ]);
                close = true;
            }
        }
        Some(Key::Char('x')) if list.chooser => {
            if let Some(subject) = list
                .items
                .get(list.selected)
                .and_then(|i| i.subject.clone())
            {
                run = Some(vec![
                    "confirm-close".into(),
                    "-t".into(),
                    describe_id(session_ws(&subject)),
                ]);
                close = true;
            }
        }
        _ => {}
    }
    view.dirty = true;
    if let Some(argv) = run {
        // An unavailable entry explains itself and the list stays open.
        if let Some(reason) = session.unavailable(&argv, &Ctx::client(client)) {
            if let Some(view) = session.views.get_mut(&client) {
                view.error(reason);
            }
            return;
        }
        if let Some(view) = session.views.get_mut(&client) {
            view.mode = Mode::Normal;
        }
        run_for(session, client, &argv);
    } else if close && let Some(view) = session.views.get_mut(&client) {
        view.mode = Mode::Normal;
    }
}

fn session_ws(subject: &AnyRef) -> &AnyRef {
    subject
}
fn describe_id(subject: &AnyRef) -> String {
    describe(subject)
}

/// A key in a prompt: a one-line editor.
pub fn prompt_key(session: &mut Session, client: ClientId, press: KeyPress) {
    let Some(view) = session.views.get_mut(&client) else {
        return;
    };
    let Mode::Prompt(prompt) = &mut view.mode else {
        return;
    };
    view.dirty = true;
    let len = prompt.text.chars().count();
    let byte = |text: &str, index: usize| {
        text.char_indices()
            .nth(index)
            .map_or(text.len(), |(i, _)| i)
    };
    if press.mods.ctrl && !press.mods.alt {
        match press.key {
            Key::Char('u') => {
                let at = byte(&prompt.text, prompt.cursor);
                prompt.text.replace_range(..at, "");
                prompt.cursor = 0;
            }
            Key::Char('a') => prompt.cursor = 0,
            Key::Char('e') => prompt.cursor = len,
            Key::Char('c') | Key::Char('g') => view.mode = Mode::Normal,
            _ => {}
        }
        return;
    }
    match press.key {
        Key::Escape => view.mode = Mode::Normal,
        Key::Enter => {
            let prompt = prompt.clone();
            view.mode = Mode::Normal;
            submit(session, client, prompt);
        }
        Key::Backspace => {
            if prompt.cursor > 0 {
                let at = byte(&prompt.text, prompt.cursor - 1);
                prompt.text.remove(at);
                prompt.cursor -= 1;
            }
        }
        Key::Delete => {
            if prompt.cursor < len {
                let at = byte(&prompt.text, prompt.cursor);
                prompt.text.remove(at);
            }
        }
        Key::Arrow(Direction::Left) => prompt.cursor = prompt.cursor.saturating_sub(1),
        Key::Arrow(Direction::Right) => prompt.cursor = (prompt.cursor + 1).min(len),
        Key::Home => prompt.cursor = 0,
        Key::End => prompt.cursor = len,
        Key::Char(c) if !press.mods.alt && !c.is_control() && prompt.text.len() < 4096 => {
            let at = byte(&prompt.text, prompt.cursor);
            prompt.text.insert(at, c);
            prompt.cursor += 1;
        }
        _ => {}
    }
}

/// Pasted text goes into a prompt, one line of it; nothing else takes pastes.
pub fn prompt_paste(session: &mut Session, client: ClientId, text: &str) {
    let Some(view) = session.views.get_mut(&client) else {
        return;
    };
    let Mode::Prompt(prompt) = &mut view.mode else {
        return;
    };
    let line: String = text
        .chars()
        .take_while(|c| *c != '\n' && *c != '\r')
        .filter(|c| !c.is_control())
        .collect();
    let byte = prompt
        .text
        .char_indices()
        .nth(prompt.cursor)
        .map_or(prompt.text.len(), |(i, _)| i);
    if prompt.text.len() + line.len() <= 4096 {
        prompt.text.insert_str(byte, &line);
        prompt.cursor += line.chars().count();
        view.dirty = true;
    }
}

fn submit(session: &mut Session, client: ClientId, prompt: Prompt) {
    match prompt.purpose {
        PromptFor::Command => {
            let argv = match crate::words::split(&prompt.text) {
                Ok(argv) if argv.is_empty() => return,
                Ok(argv) => argv,
                Err(error) => {
                    if let Some(view) = session.views.get_mut(&client) {
                        view.error(error);
                    }
                    return;
                }
            };
            run_for(session, client, &argv);
        }
        PromptFor::Rename(target) => {
            let id = match &target {
                AnyRef::Workspace(r) => match session.resolve_ws(r) {
                    Ok(w) => w.to_string(),
                    Err(error) => {
                        if let Some(view) = session.views.get_mut(&client) {
                            view.error(error);
                        }
                        return;
                    }
                },
                other => describe(other),
            };
            run_for(
                session,
                client,
                &["rename".into(), "-t".into(), id, prompt.text],
            );
        }
    }
}

/// A key while a confirmation waits: `y` runs it, `n` or Escape cancels.
pub fn confirm_key(session: &mut Session, client: ClientId, press: KeyPress) {
    let Some(view) = session.views.get_mut(&client) else {
        return;
    };
    let Mode::Confirm(confirm) = &view.mode else {
        return;
    };
    view.dirty = true;
    match plain(press) {
        Some(Key::Char('y')) | Some(Key::Char('Y')) => {
            let argv = confirm.argv.clone();
            view.mode = Mode::Normal;
            run_for(session, client, &argv);
        }
        Some(Key::Char('n')) | Some(Key::Char('N')) | Some(Key::Escape) | Some(Key::Char('q')) => {
            view.mode = Mode::Normal;
        }
        _ => {}
    }
}

/// The kind a target is.
pub fn kind_of(target: &AnyRef) -> Kind {
    match target {
        AnyRef::Pane(_) => Kind::Pane,
        AnyRef::Tab(_) => Kind::Tab,
        AnyRef::Workspace(_) => Kind::Workspace,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::ClientId;
    use crate::config::Config;
    use crate::view::Mode;

    type Outcome = Result<(), String>;

    /// A session without processes, one client attached.
    fn session() -> Result<(Session, ClientId), String> {
        let mut session = Session::new(Config::default(), "/nonexistent/fux.sock".into(), false);
        session.start()?;
        let client = session.attach(30, 100, None)?;
        Ok((session, client))
    }

    fn run(session: &mut Session, line: &str) -> Outcome {
        let outcome = session.run(&crate::words::split(line)?, &Ctx::default());
        if outcome.status == 0 {
            Ok(())
        } else {
            Err(outcome.stderr)
        }
    }

    fn mode(session: &Session, client: ClientId) -> String {
        match session.views.get(&client).map(|v| &v.mode) {
            Some(Mode::Normal) => "normal".into(),
            Some(Mode::Column { selected }) => format!("column {selected}"),
            Some(Mode::List(list)) => format!("list {} {}", list.title, list.selected),
            Some(Mode::Prompt(prompt)) => format!("prompt {}|{}", prompt.text, prompt.cursor),
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

    #[test]
    fn the_column_scrolls_within_its_bindings() -> Outcome {
        let (mut s, c) = session()?;
        let last = s.config.bindings.len() - 1;
        s.input(c, b"\x02");
        assert_eq!(mode(&s, c), "column 0");
        s.input(c, &b"\x1b[B".repeat(100));
        assert_eq!(
            mode(&s, c),
            format!("column {last}"),
            "Down stops at the last binding"
        );
        s.input(c, b"\x1b[H");
        assert_eq!(mode(&s, c), "column 0");
        s.input(c, b"\x1b[6~");
        assert_eq!(mode(&s, c), format!("column {}", list_capacity(30)));
        s.input(c, b"kkj");
        assert_eq!(mode(&s, c), format!("column {}", list_capacity(30) - 1));
        s.input(c, b"\x1b[F");
        assert_eq!(mode(&s, c), format!("column {last}"));
        // The rows are headings and bindings; the selection counts bindings.
        let bindings = column_rows(&s)
            .iter()
            .filter(|r| matches!(r, ColumnRow::Binding { .. }))
            .count();
        assert_eq!(bindings, last + 1);
        assert_eq!(
            column_selected(&s, 0),
            Some(vec!["split".to_owned(), "-h".to_owned()])
        );
        Ok(())
    }

    #[test]
    fn a_menu_acts_on_the_item_it_was_opened_for() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "split -h -t %1")?;
        run(&mut s, "select-pane -c c1 -t %2")?;
        s.input(c, b"\x02p");
        assert!(mode(&s, c).starts_with("list pane %2"), "{}", mode(&s, c));
        // Focus moves; the menu still means %2: its "close" asks about %2.
        run(&mut s, "select-pane -c c1 -t %1")?;
        s.input(c, b"\x1b[B\r");
        assert!(
            mode(&s, c).starts_with("confirm close pane %2"),
            "{}",
            mode(&s, c)
        );
        s.input(c, b"y");
        assert!(!s.panes.contains_key(&crate::layout::PaneId(2)));
        assert!(s.panes.contains_key(&crate::layout::PaneId(1)));
        Ok(())
    }

    #[test]
    fn a_list_whose_item_disappears_closes_and_never_retargets() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "split -h -t %1")?;
        run(&mut s, "select-pane -c c1 -t %2")?;
        s.input(c, b"\x02p");
        run(&mut s, "kill-pane -t %2")?;
        assert_eq!(mode(&s, c), "normal");
        assert!(notice(&s, c).contains("%2 is gone"), "{}", notice(&s, c));
        Ok(())
    }

    #[test]
    fn choosers_mark_the_current_item_and_start_on_it() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "new-tab -t +1 -n second")?;
        run(&mut s, "select-tab -c c1 -t @2")?;
        s.input(c, b"\x02T");
        let Some(Mode::List(list)) = s.views.get(&c).map(|v| &v.mode) else {
            return Err(mode(&s, c));
        };
        assert_eq!(list.selected, 1);
        let current: Vec<bool> = list.items.iter().map(|i| i.current).collect();
        assert_eq!(current, [false, true]);
        assert!(list.items.iter().all(|i| i.label.contains("— %")));
        Ok(())
    }

    #[test]
    fn a_prompt_edits_one_line() -> Outcome {
        let (mut s, c) = session()?;
        s.input(c, b"\x02:");
        s.input(c, "abc界".as_bytes());
        assert_eq!(mode(&s, c), "prompt abc界|4");
        s.input(c, b"\x1b[D\x1b[D\x7fX");
        assert_eq!(mode(&s, c), "prompt aXc界|2");
        s.input(c, b"\x1b[3~");
        assert_eq!(mode(&s, c), "prompt aX界|2");
        s.input(c, b"\x01Y\x05Z");
        assert_eq!(mode(&s, c), "prompt YaX界Z|5");
        s.input(c, b"\x15");
        assert_eq!(mode(&s, c), "prompt |0");
        s.input(c, b"\x1b[200~one\ntwo\x1b[201~");
        assert_eq!(mode(&s, c), "prompt one|3");
        s.input(c, b"\x1b");
        std::thread::sleep(crate::decode::ESCAPE_DELAY);
        s.escape(c);
        assert_eq!(mode(&s, c), "normal");
        Ok(())
    }
}
