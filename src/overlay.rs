//! The keyboard overlays: the command column, choosers, action menus, the
//! `:` prompt, rename prompts and confirmations. Each belongs to the client
//! that opened it.
use crate::command::{AnyRef, ClientId, Kind, TabId, WsRef};
use crate::keys::{Direction, Key, KeyPress};
use crate::layout::PaneId;
use crate::session::{Ctx, Session, describe};
use crate::view::{Confirm, Item, List, Mode, Prompt, PromptFor};

/// One row of the command column: a group heading, a binding, or a layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColumnRow {
    Heading(String),
    Binding {
        key: String,
        label: String,
        argv: Vec<String>,
    },
    /// A key that opens a layer, and the layer's title.
    Layer {
        key: KeyPress,
        title: String,
    },
}

/// The command column's rows for the layer at `path`: its bindings and the
/// layers inside it, grouped, groups in their order, custom groups after
/// them and `Other` last. A layer is listed once, where its first binding
/// is, under the group its command belongs to.
pub fn column_rows(session: &Session, path: &[KeyPress]) -> Vec<ColumnRow> {
    let mut entries: Vec<(String, ColumnRow)> = Vec::new();
    let mut layers: Vec<KeyPress> = Vec::new();
    for binding in &session.config.bindings {
        match binding.keys.strip_prefix(path) {
            Some([key]) => entries.push((
                binding.group(),
                ColumnRow::Binding {
                    key: key.to_string(),
                    label: crate::command::label(&binding.command),
                    argv: binding.command.clone(),
                },
            )),
            Some([key, _, ..]) if !layers.contains(key) => {
                layers.push(*key);
                entries.push((
                    binding.derived_group(),
                    ColumnRow::Layer {
                        key: *key,
                        title: binding.group(),
                    },
                ));
            }
            Some(_) | None => {}
        }
    }
    let mut groups: Vec<String> = crate::config::GROUPS
        .iter()
        .map(|g| (*g).to_owned())
        .collect();
    for (group, _) in &entries {
        if !groups.contains(group) && group != "Other" {
            groups.push(group.clone());
        }
    }
    groups.push("Other".into());
    let mut rows = Vec::new();
    for group in groups {
        let mut members = entries.iter().filter(|(g, _)| *g == group).peekable();
        if members.peek().is_none() {
            continue;
        }
        rows.push(ColumnRow::Heading(group.clone()));
        rows.extend(members.map(|(_, row)| row.clone()));
    }
    rows
}

/// The title of the layer at `path`: the group of its first binding.
pub fn layer_title(session: &Session, path: &[KeyPress]) -> Option<String> {
    session
        .config
        .bindings
        .iter()
        .find(|b| b.keys.len() > path.len() && b.keys.starts_with(path))
        .map(crate::config::Binding::group)
}

/// How many entries the column can select among in the layer at `path`.
fn column_len(session: &Session, path: &[KeyPress]) -> usize {
    column_rows(session, path)
        .iter()
        .filter(|row| !matches!(row, ColumnRow::Heading(_)))
        .count()
}

/// The column's `selected` entry in the layer at `path`.
pub fn column_selected(session: &Session, path: &[KeyPress], selected: usize) -> Option<ColumnRow> {
    column_rows(session, path)
        .into_iter()
        .filter(|row| !matches!(row, ColumnRow::Heading(_)))
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
        other @ (AnyRef::Pane(_) | AnyRef::Tab(_)) => describe(other),
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
        other @ (AnyRef::Pane(_) | AnyRef::Tab(_)) => other,
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
    let prefix = session.config.prefix;
    let Some((path, selected, rows)) = session.views.get(&client).and_then(|v| match &v.mode {
        Mode::Column { path, selected } => Some((path.clone(), *selected, v.rows)),
        Mode::Normal
        | Mode::Repeat { .. }
        | Mode::List(_)
        | Mode::Prompt(_)
        | Mode::Confirm(_)
        | Mode::Copy(_) => None,
    }) else {
        return;
    };
    let len = column_len(session, &path);
    let page = list_capacity(rows);
    let last = len.saturating_sub(1);
    // The column navigates with keys that are not letters: every letter after
    // the prefix is a binding's.
    let unmodified = press.mods.is_empty().then_some(press.key);
    let new = match unmodified {
        _ if press == prefix => {
            // The prefix, at any depth, sends it to the pane.
            set_mode(session, client, Mode::Normal);
            send_key(session, client, prefix);
            return;
        }
        Some(Key::Arrow(Direction::Up)) => selected.saturating_sub(1),
        // Moves stop at the first and last entries.
        Some(Key::Arrow(Direction::Down)) => selected.saturating_add(1).min(last),
        Some(Key::PageUp) => selected.saturating_sub(page),
        Some(Key::PageDown) => selected.saturating_add(page).min(last),
        Some(Key::Home) => 0,
        Some(Key::End) => last,
        Some(Key::Escape) => {
            set_mode(session, client, Mode::Normal);
            return;
        }
        Some(Key::Enter) => {
            match column_selected(session, &path, selected) {
                Some(ColumnRow::Binding { argv, .. }) => {
                    set_mode(session, client, Mode::Normal);
                    run_entry(session, client, &argv);
                }
                Some(ColumnRow::Layer { key, .. }) => follow(session, client, &path, key),
                Some(ColumnRow::Heading(_)) | None => set_mode(session, client, Mode::Normal),
            }
            return;
        }
        _ => {
            follow(session, client, &path, press);
            return;
        }
    };
    set_mode(
        session,
        client,
        Mode::Column {
            path,
            selected: new,
        },
    );
}

/// A key typed in the layer at `path`: it runs its binding, entering the
/// layer's repeat mode if the binding repeats; opens the layer it starts;
/// or, unbound, leaves the column open, saying so.
fn follow(session: &mut Session, client: ClientId, path: &[KeyPress], press: KeyPress) {
    let mut keys = path.to_vec();
    keys.push(crate::config::folded(press));
    let bindings = &session.config.bindings;
    if let Some(binding) = bindings.iter().find(|b| b.keys == keys) {
        let (argv, repeat) = (binding.command.clone(), binding.repeat);
        let mode = if repeat {
            Mode::Repeat {
                path: path.to_vec(),
            }
        } else {
            Mode::Normal
        };
        set_mode(session, client, mode);
        run_entry(session, client, &argv);
    } else if bindings
        .iter()
        .any(|b| b.keys.len() > keys.len() && b.keys.starts_with(&keys))
    {
        set_mode(
            session,
            client,
            Mode::Column {
                path: keys,
                selected: 0,
            },
        );
    } else if let Some(view) = session.views.get_mut(&client) {
        let prefix = session.config.prefix;
        view.error(format!(
            "{prefix} {} is not bound",
            crate::config::keys_text(&keys)
        ));
    }
}

/// A key in a repeat mode: one of its layer's keys runs its binding again,
/// without the prefix; Esc or Enter leaves; the prefix leaves and opens the
/// column; any other key leaves, not reaching the pane, and says so.
pub fn repeat_key(session: &mut Session, client: ClientId, press: KeyPress) {
    let prefix = session.config.prefix;
    let Some(path) = session.views.get(&client).and_then(|v| match &v.mode {
        Mode::Repeat { path } => Some(path.clone()),
        Mode::Normal
        | Mode::Column { .. }
        | Mode::List(_)
        | Mode::Prompt(_)
        | Mode::Confirm(_)
        | Mode::Copy(_) => None,
    }) else {
        return;
    };
    if press == prefix {
        set_mode(
            session,
            client,
            Mode::Column {
                path: Vec::new(),
                selected: 0,
            },
        );
        return;
    }
    if matches!(plain(press), Some(Key::Escape | Key::Enter)) {
        set_mode(session, client, Mode::Normal);
        return;
    }
    let mut keys = path.clone();
    keys.push(crate::config::folded(press));
    let found = session
        .config
        .bindings
        .iter()
        .find(|b| b.keys == keys)
        .map(|b| (b.command.clone(), b.repeat));
    match found {
        Some((argv, repeat)) => {
            let mode = if repeat {
                Mode::Repeat { path }
            } else {
                Mode::Normal
            };
            set_mode(session, client, mode);
            run_entry(session, client, &argv);
        }
        None => {
            let title = layer_title(session, &path).unwrap_or_default();
            set_mode(session, client, Mode::Normal);
            if let Some(view) = session.views.get_mut(&client) {
                view.info(format!("{title} ended: {press} is not one of its keys"));
            }
        }
    }
}

fn set_mode(session: &mut Session, client: ClientId, mode: Mode) {
    if let Some(view) = session.views.get_mut(&client) {
        view.mode = mode;
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
        // Moves stop at the first and last items.
        Some(Key::Arrow(Direction::Down)) | Some(Key::Char('j')) => {
            list.selected = list.selected.saturating_add(1).min(last)
        }
        Some(Key::PageUp) => list.selected = list.selected.saturating_sub(page),
        Some(Key::PageDown) => list.selected = list.selected.saturating_add(page).min(last),
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

/// `text` with the `remove` chars from char `at` replaced by `insert`:
/// positions count chars, so no edit can fall inside one. A prompt's text is
/// at most 4096 bytes, so building it afresh costs nothing.
fn splice(text: &str, at: usize, remove: usize, insert: &str) -> String {
    text.chars()
        .take(at)
        .chain(insert.chars())
        .chain(text.chars().skip(at).skip(remove))
        .collect()
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
    if press.mods.ctrl && !press.mods.alt {
        match press.key {
            Key::Char('u') => {
                prompt.text = splice(&prompt.text, 0, prompt.cursor, "");
                prompt.cursor = 0;
            }
            Key::Char('a') => prompt.cursor = 0,
            Key::Char('e') => prompt.cursor = len,
            Key::Char('c') | Key::Char('g') => view.mode = Mode::Normal,
            Key::Char(_)
            | Key::Enter
            | Key::Tab
            | Key::Escape
            | Key::Backspace
            | Key::Delete
            | Key::Insert
            | Key::Arrow(_)
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
            | Key::F(_) => {}
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
            if let Some(before) = prompt.cursor.checked_sub(1) {
                prompt.text = splice(&prompt.text, before, 1, "");
                prompt.cursor = before;
            }
        }
        Key::Delete => {
            if prompt.cursor < len {
                prompt.text = splice(&prompt.text, prompt.cursor, 1, "");
            }
        }
        Key::Arrow(Direction::Left) => prompt.cursor = prompt.cursor.saturating_sub(1),
        Key::Arrow(Direction::Right) => prompt.cursor = prompt.cursor.saturating_add(1).min(len),
        Key::Home => prompt.cursor = 0,
        Key::End => prompt.cursor = len,
        Key::Char(c) if !press.mods.alt && !c.is_control() && prompt.text.len() < 4096 => {
            let mut buffer = [0u8; 4];
            prompt.text = splice(&prompt.text, prompt.cursor, 0, c.encode_utf8(&mut buffer));
            // Exact: the text is under 4096 bytes.
            prompt.cursor = prompt.cursor.saturating_add(1);
        }
        Key::Char(_)
        | Key::Tab
        | Key::Insert
        | Key::Arrow(_)
        | Key::PageUp
        | Key::PageDown
        | Key::F(_) => {}
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
    if prompt
        .text
        .len()
        .checked_add(line.len())
        .is_some_and(|len| len <= 4096)
    {
        prompt.text = splice(&prompt.text, prompt.cursor, 0, &line);
        // Exact: the text is at most 4096 bytes.
        prompt.cursor = prompt.cursor.saturating_add(line.chars().count());
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
                other @ (AnyRef::Pane(_) | AnyRef::Tab(_)) => describe(other),
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
            Some(Mode::Column { path, selected }) if path.is_empty() => {
                format!("column {selected}")
            }
            Some(Mode::Column { path, selected }) => {
                format!("column {} {selected}", crate::config::keys_text(path))
            }
            Some(Mode::Repeat { path }) => {
                format!("repeat {}", crate::config::keys_text(path))
            }
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

    /// Prompt edits count chars, so none falls inside one: what Ctrl-U,
    /// Backspace, Delete, typing and a paste do, on text with wide chars.
    #[test]
    fn prompt_edits_count_chars_not_bytes() {
        for (at, remove, insert, edited) in [
            (0, 2, "", "llo界"),
            (1, 1, "", "hllo界"),
            (4, 1, "", "héll界"),
            (5, 1, "", "héllo"),
            (6, 1, "", "héllo界"),
            (2, 0, "ü", "héüllo界"),
            (6, 0, "x", "héllo界x"),
            (9, 0, "x", "héllo界x"),
            (3, 0, "p a", "hélp alo界"),
        ] {
            assert_eq!(
                splice("héllo界", at, remove, insert),
                edited,
                "{at} {remove} {insert:?}"
            );
        }
    }

    #[test]
    fn the_column_scrolls_within_its_bindings() -> Outcome {
        let (mut s, c) = session()?;
        // Its entries: the bindings and layers right after the prefix.
        let entries = column_rows(&s, &[])
            .into_iter()
            .filter(|r| !matches!(r, ColumnRow::Heading(_)))
            .count();
        let last = entries.saturating_sub(1);
        s.input(c, b"\x02");
        assert_eq!(mode(&s, c), "column 0");
        let downs: Vec<u8> = std::iter::repeat_n(&b"\x1b[B"[..], 100)
            .flatten()
            .copied()
            .collect();
        s.input(c, &downs);
        assert_eq!(
            mode(&s, c),
            format!("column {last}"),
            "Down stops at the last entry"
        );
        s.input(c, b"\x1b[H");
        assert_eq!(mode(&s, c), "column 0");
        // A page down, or to the last entry if that is nearer.
        let page = list_capacity(30).min(last);
        s.input(c, b"\x1b[6~");
        assert_eq!(mode(&s, c), format!("column {page}"));
        s.input(c, b"\x1b[A\x1b[A\x1b[B");
        assert_eq!(mode(&s, c), format!("column {}", page.saturating_sub(1)));
        s.input(c, b"\x1b[F");
        assert_eq!(mode(&s, c), format!("column {last}"));
        // Panes come first.
        assert!(matches!(
            column_selected(&s, &[], 0),
            Some(ColumnRow::Binding { argv, .. }) if argv == ["split", "-h"]
        ));
        // Letters are bindings, not moves: `j` focuses down and closes it.
        s.input(c, b"\x1b[Hj");
        assert_eq!(mode(&s, c), "normal");
        Ok(())
    }

    /// A layer and a repeat mode, bound here rather than by default.
    fn with_layers(s: &mut Session) -> Outcome {
        run(s, "bind g n new-tab")?;
        run(s, "bind -g Grow -r y l resize-pane -R")?;
        run(s, "bind -g Grow -r y h resize-pane -L")
    }

    fn width(s: &Session, pane: u32) -> u16 {
        s.panes
            .get(&crate::layout::PaneId(pane))
            .map_or(0, |p| p.screen().size().1)
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
            .get_mut(&crate::layout::PaneId(pane))
            .map(|p| p.input.drain_all())
            .unwrap_or_default()
    }

    #[test]
    fn a_layer_runs_its_binding_on_the_next_key() -> Outcome {
        let (mut s, c) = session()?;
        with_layers(&mut s)?;
        // Right after the prefix, the layer is one entry, under its
        // command's group, titled by its first binding's group.
        let layer = ColumnRow::Layer {
            key: KeyPress::char('g'),
            title: "Tabs".into(),
        };
        assert!(column_rows(&s, &[]).contains(&layer));
        s.input(c, b"\x02g");
        assert_eq!(mode(&s, c), "column g 0");
        // The column shows the keys so far and the layer's title, and the
        // bar what has been typed.
        let text = screen_text(&s, c)?;
        assert!(text.contains("C-b g: Tabs"), "{text}");
        assert!(text.contains("C-b g …"), "{text}");
        assert_eq!(
            column_rows(&s, &[KeyPress::char('g')]),
            vec![
                ColumnRow::Heading("Tabs".into()),
                ColumnRow::Binding {
                    key: "n".into(),
                    label: crate::command::label(&["new-tab".to_owned()]),
                    argv: vec!["new-tab".into()],
                },
            ]
        );
        s.input(c, b"n");
        assert_eq!(mode(&s, c), "normal");
        assert_eq!(s.workspaces.first().map(|w| w.tabs.len()), Some(2));
        // Enter on the layer's entry opens it too.
        let at = column_rows(&s, &[])
            .into_iter()
            .filter(|row| !matches!(row, ColumnRow::Heading(_)))
            .position(|row| row == layer)
            .ok_or("the layer is listed")?;
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
        run(&mut s, "split -h -t %1")?;
        run(&mut s, "select-pane -c c1 -t %1")?;
        let start = width(&s, 1);
        s.input(c, b"\x02y");
        assert_eq!(mode(&s, c), "column y 0");
        s.input(c, b"l");
        assert_eq!(mode(&s, c), "repeat y");
        // No prefix: the mode's keys run again and again.
        s.input(c, b"ll");
        assert_eq!(start.checked_add(3), Some(width(&s, 1)));
        s.input(c, b"h");
        assert_eq!(start.checked_add(2), Some(width(&s, 1)));
        // The bar names the mode and its keys.
        let grid = crate::render::compose(&s, c).ok_or("a screen")?;
        let bar = grid.row_text(grid.rows.saturating_sub(1));
        assert!(bar.ends_with("GROW  l h · Esc"), "{bar}");
        s.input(c, b"\r");
        assert_eq!(mode(&s, c), "normal");
        // Esc leaves too, once its deadline passes.
        s.input(c, b"\x02yl\x1b");
        std::thread::sleep(crate::decode::ESCAPE_DELAY);
        s.escape(c);
        assert_eq!(mode(&s, c), "normal");
        assert_eq!(start.checked_add(3), Some(width(&s, 1)));
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
        assert_eq!(notice(&s, c), "Grow ended: f is not one of its keys");
        assert!(queued(&mut s, 1).is_empty());
        // The prefix leaves it for the column.
        s.input(c, b"\x02yl\x02");
        assert_eq!(mode(&s, c), "column 0");
        Ok(())
    }

    #[test]
    fn list_keys_shows_sequences_and_repeats() -> Outcome {
        let (mut s, _) = session()?;
        with_layers(&mut s)?;
        let out = s.run(&["list-keys".to_owned()], &Ctx::default()).stdout;
        assert!(out.contains("\n     g n  new-tab\n"), "{out}");
        assert!(
            out.contains("\n     y l  resize-pane -R (repeats)\n"),
            "{out}"
        );
        Ok(())
    }

    /// A session with room for every command to act: two workspaces, the
    /// first with two tabs, its first with three panes, 1 | (2 / 3), the
    /// client on %2.
    fn busy() -> Result<(Session, ClientId), String> {
        let (mut s, c) = session()?;
        run(&mut s, "split -h -t %1")?;
        run(&mut s, "split -v -t %2")?;
        run(&mut s, "new-tab -t +1")?;
        run(&mut s, "new-workspace")?;
        run(&mut s, "select-pane -c c1 -t %2")?;
        Ok((s, c))
    }

    /// What a user can tell apart: every workspace, tab, pane and client,
    /// the client's mode, and its notice.
    fn state(s: &mut Session, c: ClientId) -> String {
        let ls = s.run(&["ls".to_owned()], &Ctx::default()).stdout;
        format!("{ls}{}\n{}", mode(s, c), notice(s, c))
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

    /// Typing a default binding's keys does what running its command does;
    /// a repeating one also enters its mode.
    #[test]
    fn every_default_binding_runs_its_command() -> Outcome {
        let bindings = Config::default().bindings;
        assert!(!bindings.is_empty());
        for binding in bindings {
            let keys: String = binding.keys.iter().map(KeyPress::to_string).collect();
            let (mut typed, c) = busy()?;
            typed.input(c, &prefixed(&keys));
            if binding.repeat {
                let layer = binding
                    .keys
                    .split_last()
                    .map(|(_, l)| l)
                    .unwrap_or_default();
                let expected = format!("repeat {}", crate::config::keys_text(layer));
                assert_eq!(mode(&typed, c), expected, "{keys}");
                typed.input(c, b"\r");
            }
            let (mut ran, d) = busy()?;
            run_entry(&mut ran, d, &binding.command);
            assert_eq!(state(&mut typed, c), state(&mut ran, d), "{keys}");
        }
        Ok(())
    }

    #[test]
    fn keys_after_the_prefix_are_letters_in_either_case() -> Outcome {
        let (mut lower, c) = busy()?;
        lower.input(c, &prefixed("tn"));
        let (mut upper, d) = busy()?;
        upper.input(d, &prefixed("TN"));
        assert_eq!(state(&mut lower, c), state(&mut upper, d));
        assert_eq!(lower.workspaces.first().map(|w| w.tabs.len()), Some(3));
        // A letter with Ctrl is no binding's.
        lower.input(c, &prefixed("\x14"));
        assert_eq!(notice(&lower, c), "C-b C-t is not bound");
        Ok(())
    }

    #[test]
    fn resize_mode_repeats_its_keys_until_esc() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "split -h -t %1")?;
        run(&mut s, "select-pane -c c1 -t %1")?;
        let start = width(&s, 1);
        s.input(c, &prefixed("rlll"));
        assert_eq!(mode(&s, c), "repeat r");
        assert_eq!(start.checked_add(3), Some(width(&s, 1)));
        escape(&mut s, c);
        assert_eq!(mode(&s, c), "normal");
        // After Esc, `l` is the pane's again.
        let _ = queued(&mut s, 1);
        s.input(c, b"l");
        assert_eq!(queued(&mut s, 1), b"l");
        assert_eq!(start.checked_add(3), Some(width(&s, 1)));
        Ok(())
    }

    /// The panes of the client's tab, left to right.
    fn pane_order(s: &mut Session) -> Vec<String> {
        let ls = s.run(&["ls".to_owned()], &Ctx::default()).stdout;
        ls.lines()
            .filter_map(|l| l.trim_start().split(' ').next())
            .filter(|w| w.starts_with('%'))
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn move_mode_moves_a_pane_until_esc() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "split -h -t %1")?;
        run(&mut s, "split -h -t %2")?;
        run(&mut s, "select-pane -c c1 -t %3")?;
        assert_eq!(pane_order(&mut s), ["%1", "%2", "%3"]);
        s.input(c, &prefixed("mhh"));
        escape(&mut s, c);
        assert_eq!(mode(&s, c), "normal");
        assert_eq!(pane_order(&mut s), ["%3", "%1", "%2"]);
        Ok(())
    }

    #[test]
    fn the_tab_layer_reorders_in_a_repeat_mode() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "new-tab -t +1 -n two")?;
        run(&mut s, "new-tab -t +1 -n three")?;
        run(&mut s, "select-tab -c c1 -t @1")?;
        s.input(c, &prefixed("tmll"));
        assert_eq!(mode(&s, c), "repeat t m");
        escape(&mut s, c);
        let order: Vec<u32> = s
            .workspaces
            .first()
            .map(|w| w.tabs.iter().map(|t| t.id.0).collect())
            .unwrap_or_default();
        assert_eq!(order, [2, 3, 1]);
        Ok(())
    }

    #[test]
    fn a_menu_acts_on_the_item_it_was_opened_for() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "split -h -t %1")?;
        run(&mut s, "select-pane -c c1 -t %2")?;
        s.input(c, b"\x02a");
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
        s.input(c, b"\x02a");
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
        s.input(c, b"\x02tg");
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
        s.input(c, b"\x02e");
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
