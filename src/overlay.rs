//! The keyboard overlays: the command column, choosers, action menus, the
//! command prompt, rename prompts and confirmations. Each belongs to the client
//! that opened it.
use crate::command::{AnyRef, ClientId, Command, Kind, MoveTo, Pick, SwapWith, Usage, WsRef};
use crate::config::Binding;
use crate::keys::{Direction, Key, KeyPress};
use crate::layout::{Node, PaneId};
use crate::session::{Ctx, Session, describe};
use crate::view::{Confirm, Item, List, Mode, Prompt, PromptFor};

/// One row of the command column: a group heading, a binding, or a layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColumnRow {
    Heading(String),
    Binding {
        key: KeyPress,
        label: String,
        command: Command,
    },
    /// A key that opens a layer, and the layer's title.
    Layer {
        key: KeyPress,
        title: String,
    },
}

/// An entry of the command column: a binding of its layer, or a key that
/// opens a layer inside it, with the layer's first binding.
#[derive(Clone, Copy)]
enum Entry<'a> {
    Binding(KeyPress, &'a Binding),
    Layer(KeyPress, &'a Binding),
}

impl<'a> Entry<'a> {
    /// The group it is listed under: a binding's own, or the group of the
    /// command that a layer's first binding runs.
    fn group(self) -> &'a str {
        match self {
            Entry::Binding(_, binding) => binding.group(),
            Entry::Layer(_, first) => first.derived_group(),
        }
    }

    fn row(self) -> ColumnRow {
        match self {
            Entry::Binding(key, binding) => ColumnRow::Binding {
                key,
                label: crate::command::label(&binding.command),
                command: binding.parsed.clone(),
            },
            Entry::Layer(key, first) => ColumnRow::Layer {
                key,
                title: first.group().to_owned(),
            },
        }
    }
}

/// The column's entries for the layer at `path`, in the order of the
/// bindings, found without building their rows. A layer is an entry once,
/// where its first binding is.
fn entries<'a>(session: &'a Session, path: &'a [KeyPress]) -> impl Iterator<Item = Entry<'a>> {
    let bindings = &session.config.bindings;
    bindings.iter().enumerate().filter_map(move |(i, binding)| {
        match binding.keys.strip_prefix(path) {
            Some([key]) => Some(Entry::Binding(*key, binding)),
            Some([key, _, ..]) if !bindings.iter().take(i).any(|b| opens(b, path, key)) => {
                Some(Entry::Layer(*key, binding))
            }
            Some(_) | None => None,
        }
    })
}

/// Whether `binding` is in the layer that `key` opens in the layer at `path`.
fn opens(binding: &Binding, path: &[KeyPress], key: &KeyPress) -> bool {
    matches!(binding.keys.strip_prefix(path), Some([k, _, ..]) if k == key)
}

/// The column's entries in its order, with their groups: groups in their
/// order, custom groups after them and `Other` last.
fn ordered<'a>(session: &'a Session, path: &'a [KeyPress]) -> Vec<(&'a str, Entry<'a>)> {
    let entries: Vec<(&str, Entry)> = entries(session, path).map(|e| (e.group(), e)).collect();
    let mut groups: Vec<&str> = crate::config::GROUPS.to_vec();
    for (group, _) in &entries {
        if !groups.contains(group) && *group != "Other" {
            groups.push(group);
        }
    }
    groups.push("Other");
    groups
        .iter()
        .flat_map(|group| entries.iter().filter(move |(g, _)| g == group).cloned())
        .collect()
}

/// The command column's rows for the layer at `path`: its bindings and the
/// layers inside it, grouped, groups in their order, custom groups after
/// them and `Other` last. A layer is listed once, where its first binding
/// is, under the group its command belongs to.
pub fn column_rows(session: &Session, path: &[KeyPress]) -> Vec<ColumnRow> {
    let mut rows = Vec::new();
    let mut heading = None;
    for (group, entry) in ordered(session, path) {
        if heading != Some(group) {
            rows.push(ColumnRow::Heading(group.to_owned()));
            heading = Some(group);
        }
        rows.push(entry.row());
    }
    rows
}

/// The title of the layer at `path`: the group of its first binding.
pub fn layer_title<'a>(session: &'a Session, path: &[KeyPress]) -> Option<&'a str> {
    session
        .config
        .bindings
        .iter()
        .find(|b| b.keys.len() > path.len() && b.keys.starts_with(path))
        .map(crate::config::Binding::group)
}

/// How many entries the column can select among in the layer at `path`.
pub(crate) fn column_len(session: &Session, path: &[KeyPress]) -> usize {
    entries(session, path).count()
}

/// The column's `selected` entry in the layer at `path`; the other entries'
/// rows are not built.
pub fn column_selected(session: &Session, path: &[KeyPress], selected: usize) -> Option<ColumnRow> {
    ordered(session, path)
        .into_iter()
        .nth(selected)
        .map(|(_, entry)| entry.row())
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
    let (kind, id, command) = match &target {
        AnyRef::Pane(p) => (
            "pane",
            p.to_string(),
            Command::KillPane { target: Some(*p) },
        ),
        AnyRef::Tab(t) => ("tab", t.to_string(), Command::KillTab { target: Some(*t) }),
        AnyRef::Workspace(r) => {
            let ws = session.resolve_ws(r)?;
            let command = Command::KillWorkspace {
                target: Some(WsRef::Id(ws)),
            };
            ("workspace", ws.to_string(), command)
        }
    };
    let name = session.name_of(&target);
    let question = format!("close {kind} {id} {name}?");
    let view = session.views.get_mut(&client).ok_or("no such client")?;
    view.mode = Mode::Confirm(Confirm {
        question,
        command,
        about: target,
    });
    Ok(String::new())
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
pub fn open_menu(
    session: &mut Session,
    client: ClientId,
    target: AnyRef,
) -> Result<String, String> {
    // What the menu is for, which every item names.
    let about = match target {
        AnyRef::Workspace(r) => AnyRef::Workspace(WsRef::Id(session.resolve_ws(&r)?)),
        other @ (AnyRef::Pane(_) | AnyRef::Tab(_)) => other,
    };
    let kind = kind_of(&about);
    let name = session.name_of(&about);
    let title = format!("{} {} {name}", kind.name(), describe(&about));
    let target = Some(about.clone());
    let reorder = |forward| Command::Reorder {
        kind,
        target: target.clone(),
        forward,
    };
    let mut items = vec![
        item(
            "rename",
            Command::RenamePrompt {
                client: None,
                kind,
                target: target.clone(),
            },
        ),
        item(
            "close",
            Command::ConfirmClose {
                client: None,
                kind,
                target: target.clone(),
            },
        ),
    ];
    match &about {
        &AnyRef::Pane(p) => items.extend([
            item(
                "terminate the running command",
                Command::Terminate { target: Some(p) },
            ),
            item(
                "swap with…",
                Command::ChoosePane {
                    client: None,
                    target: Some(p),
                },
            ),
            item(
                "move to tab…",
                Command::ChooseTab {
                    client: None,
                    moving: Some(p),
                    moving_now: false,
                },
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
                Command::ChooseWorkspace {
                    client: None,
                    moving: Some(p),
                    moving_now: false,
                },
            ),
            item(
                "move to a new workspace",
                Command::MovePane {
                    target: Some(p),
                    to: MoveTo::NewWorkspace,
                },
            ),
        ]),
        &AnyRef::Tab(tab) => {
            let ws = session
                .find_tab(tab)
                .and_then(|(w, _)| session.workspaces.get(w))
                .map(|w| WsRef::Id(w.id));
            items.push(item(
                "new tab",
                Command::NewTab {
                    target: ws,
                    name: None,
                    cmd: Vec::new(),
                },
            ));
        }
        AnyRef::Workspace(_) => items.push(item(
            "new workspace",
            Command::NewWorkspace {
                name: None,
                cmd: Vec::new(),
            },
        )),
    }
    items.extend([
        item("reorder previous", reorder(false)),
        item("reorder next", reorder(true)),
    ]);
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

/// The names of the panes in `roots`, for a chooser row, shortened.
fn pane_names<'a>(session: &Session, roots: impl IntoIterator<Item = &'a Node>) -> String {
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
    session: &mut Session,
    client: ClientId,
    moving: Option<PaneId>,
) -> Result<String, String> {
    let view = session.views.get(&client).ok_or("no such client")?;
    let current = view.tab();
    let ws = session.workspace(view.workspace).ok_or("no workspace")?;
    let items = ws
        .tabs
        .iter()
        .map(|tab| {
            let (id, name) = (tab.id, &tab.name);
            Item {
                label: format!("{id} {name} — {}", pane_names(session, &tab.root)),
                command: match moving {
                    Some(p) => Command::MovePane {
                        target: Some(p),
                        to: MoveTo::Tab(id),
                    },
                    None => Command::SelectTab {
                        client: None,
                        pick: Pick::Id(id),
                    },
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
            let roots = ws.tabs.iter().filter_map(|t| t.root.as_ref());
            Item {
                label: format!("{} {} — {}", ws.id, ws.name, pane_names(session, roots)),
                command: match moving {
                    Some(p) => Command::MovePane {
                        target: Some(p),
                        to: MoveTo::Workspace(WsRef::Id(ws.id)),
                    },
                    None => Command::SelectWorkspace {
                        client: None,
                        pick: Pick::Id(WsRef::Id(ws.id)),
                    },
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
    let mut items: Vec<Item> = Vec::new();
    if let Some(root) = session.tab(tab).and_then(|t| t.root.as_ref()) {
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

/// Runs a command for a client: output and errors become its notice.
pub fn run_for(session: &mut Session, client: ClientId, command: &Command) {
    let outcome = session.run_command(command, &Ctx::client(client));
    if let Some(view) = session.views.get_mut(&client) {
        if outcome.status != 0 {
            view.error(outcome.stderr.lines().next().unwrap_or("failed").to_owned());
        } else if let Some(line) = outcome.stdout.lines().find(|l| !l.trim().is_empty())
            && !matches!(
                command,
                Command::Split { .. }
                    | Command::NewTab { .. }
                    | Command::NewWorkspace { .. }
                    | Command::MovePane { .. }
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

/// Runs a command line typed for a client, as `run_for` runs a command; one
/// that does not parse says why.
fn run_line(session: &mut Session, client: ClientId, argv: &[String]) {
    match crate::command::parse(argv) {
        Ok(command) => run_for(session, client, &command),
        Err(Usage(message)) => {
            if let Some(view) = session.views.get_mut(&client) {
                view.error(message.lines().next().unwrap_or("failed").to_owned());
            }
        }
    }
}

/// Runs an entry of a list or the column, unless it cannot run now, in
/// which case the reason is shown and nothing happens.
fn run_entry(session: &mut Session, client: ClientId, command: &Command) {
    if let Some(reason) = session.unavailable(command, &Ctx::client(client)) {
        if let Some(view) = session.views.get_mut(&client) {
            view.error(reason);
        }
        return;
    }
    run_for(session, client, command);
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
                Some(ColumnRow::Binding { command, .. }) => {
                    set_mode(session, client, Mode::Normal);
                    run_entry(session, client, &command);
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
        let (command, repeat) = (binding.parsed.clone(), binding.repeat);
        let mode = if repeat {
            Mode::Repeat {
                path: path.to_vec(),
            }
        } else {
            Mode::Normal
        };
        set_mode(session, client, mode);
        run_entry(session, client, &command);
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
        .map(|b| (b.parsed.clone(), b.repeat));
    match found {
        Some((command, repeat)) => {
            let mode = if repeat {
                Mode::Repeat { path }
            } else {
                Mode::Normal
            };
            set_mode(session, client, mode);
            run_entry(session, client, &command);
        }
        None => {
            let title = layer_title(session, &path).unwrap_or_default().to_owned();
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
    let mut run: Option<Command> = None;
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
            run = list.items.get(list.selected).map(|i| i.command.clone());
            close = run.is_some();
        }
        Some(Key::Char('r')) if list.chooser => {
            if let Some(subject) = list
                .items
                .get(list.selected)
                .and_then(|i| i.subject.clone())
            {
                run = Some(Command::RenamePrompt {
                    client: None,
                    kind: kind_of(&subject),
                    target: Some(subject),
                });
                close = true;
            }
        }
        Some(Key::Char('x')) if list.chooser => {
            if let Some(subject) = list
                .items
                .get(list.selected)
                .and_then(|i| i.subject.clone())
            {
                run = Some(Command::ConfirmClose {
                    client: None,
                    kind: kind_of(&subject),
                    target: Some(subject),
                });
                close = true;
            }
        }
        _ => {}
    }
    view.dirty = true;
    if let Some(command) = run {
        // An unavailable entry explains itself and the list stays open.
        if let Some(reason) = session.unavailable(&command, &Ctx::client(client)) {
            if let Some(view) = session.views.get_mut(&client) {
                view.error(reason);
            }
            return;
        }
        if let Some(view) = session.views.get_mut(&client) {
            view.mode = Mode::Normal;
        }
        run_for(session, client, &command);
    } else if close && let Some(view) = session.views.get_mut(&client) {
        view.mode = Mode::Normal;
    }
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
        // Text: a letter with Ctrl or Alt types nothing.
        Key::Char(c)
            if !press.mods.ctrl
                && !press.mods.alt
                && !c.is_control()
                && prompt.text.len() < 4096 =>
        {
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
            run_line(session, client, &argv);
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
            run_line(
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
            let command = confirm.command.clone();
            view.mode = Mode::Normal;
            run_for(session, client, &command);
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
            Some(ColumnRow::Binding {
                command: Command::Split {
                    axis: crate::layout::Axis::Horizontal,
                    target: None,
                    cmd,
                },
                ..
            }) if cmd.is_empty()
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
                    key: KeyPress::char('n'),
                    label: crate::command::label(&["new-tab".to_owned()]),
                    command: Command::NewTab {
                        target: None,
                        name: None,
                        cmd: Vec::new(),
                    },
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

    /// Bindings can change under a client from the command line: a column
    /// or repeat mode whose layer goes closes, as a list whose item goes
    /// does, and a column's selection stays within it.
    #[test]
    fn a_layer_unbound_under_a_client_closes_its_column_or_mode() -> Outcome {
        let (mut s, c) = session()?;
        with_layers(&mut s)?;
        s.input(c, b"\x02g");
        run(&mut s, "unbind g")?;
        assert_eq!(mode(&s, c), "normal");
        assert_eq!(notice(&s, c), "closed: the layer C-b g is gone");
        s.input(c, b"\x02yl");
        assert_eq!(mode(&s, c), "repeat y");
        run(&mut s, "unbind y h")?;
        assert_eq!(mode(&s, c), "repeat y");
        run(&mut s, "bind y l zoom")?;
        assert_eq!(mode(&s, c), "normal");
        assert_eq!(notice(&s, c), "closed: the repeat mode C-b y is gone");
        s.input(c, b"\x02\x1b[F");
        let last = column_len(&s, &[]).saturating_sub(1);
        assert_eq!(mode(&s, c), format!("column {last}"));
        run(&mut s, "unbind d")?;
        assert_eq!(mode(&s, c), format!("column {}", last.saturating_sub(1)));
        run(&mut s, "unbind-all")?;
        assert_eq!(mode(&s, c), "column 0");
        Ok(())
    }

    /// The server settles after every read of a pane's output: an open
    /// column keeps its layer and its selection, counted without building
    /// its rows, and shows the output behind it.
    #[test]
    fn a_column_stays_open_while_a_pane_writes() -> Outcome {
        let (mut s, c) = session()?;
        with_layers(&mut s)?;
        for path in [&[][..], &[KeyPress::char('t')], &[KeyPress::char('y')]] {
            let entries = column_rows(&s, path)
                .into_iter()
                .filter(|r| !matches!(r, ColumnRow::Heading(_)))
                .count();
            assert_eq!(column_len(&s, path), entries, "{path:?}");
        }
        s.input(c, b"\x02\x1b[B\x1b[B\x1b[B");
        assert_eq!(mode(&s, c), "column 3");
        let selected = column_selected(&s, &[], 3);
        assert!(selected.is_some());
        for i in 0..50 {
            s.output(
                crate::layout::PaneId(1),
                format!("output {i}\r\n").as_bytes(),
            );
            s.settle();
        }
        assert_eq!(mode(&s, c), "column 3");
        assert_eq!(column_selected(&s, &[], 3), selected);
        let text = screen_text(&s, c)?;
        assert!(
            text.contains("output 49") && text.contains("Commands"),
            "{text}"
        );
        // In a layer too.
        s.input(c, b"\x1b");
        s.escape(c);
        s.input(c, b"\x02t\x1b[B");
        assert_eq!(mode(&s, c), "column t 1");
        s.output(crate::layout::PaneId(1), b"more\r\n");
        s.settle();
        assert_eq!(mode(&s, c), "column t 1");
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
            run_entry(&mut ran, d, &binding.parsed);
            assert_eq!(state(&mut typed, c), state(&mut ran, d), "{keys}");
        }
        Ok(())
    }

    /// Menus, choosers and confirmations build their commands, typed,
    /// with every target given: each is what the command line they used to
    /// hold parses to.
    #[test]
    fn listed_commands_are_what_their_lines_parse_to() -> Outcome {
        let (mut s, c) = busy()?;
        let parsed = |line: &str| -> Result<Command, String> {
            crate::command::parse(&crate::words::split(line)?).map_err(|u| u.0)
        };
        let listed = |s: &Session| match s.views.get(&c).map(|v| &v.mode) {
            Some(Mode::List(list)) => list.items.iter().map(|i| i.command.clone()).collect(),
            Some(Mode::Confirm(confirm)) => vec![confirm.command.clone()],
            _ => Vec::new(),
        };
        for (open, lines) in [
            (
                "menu -c c1 pane -t %2",
                &[
                    "rename-prompt pane -t %2",
                    "confirm-close pane -t %2",
                    "terminate -t %2",
                    "choose-pane -t %2",
                    "choose-tab -t %2",
                    "move-pane -t %2 --to new-tab",
                    "choose-workspace -t %2",
                    "move-pane -t %2 --to new-workspace",
                    "reorder pane --previous -t %2",
                    "reorder pane --next -t %2",
                ][..],
            ),
            (
                "menu -c c1 tab -t @1",
                &[
                    "rename-prompt tab -t @1",
                    "confirm-close tab -t @1",
                    "new-tab -t +1",
                    "reorder tab --previous -t @1",
                    "reorder tab --next -t @1",
                ],
            ),
            (
                "menu -c c1 workspace -t main",
                &[
                    "rename-prompt workspace -t +1",
                    "confirm-close workspace -t +1",
                    "new-workspace",
                    "reorder workspace --previous -t +1",
                    "reorder workspace --next -t +1",
                ],
            ),
            (
                "choose-tab -c c1",
                &["select-tab -t @1", "select-tab -t @2"],
            ),
            (
                "choose-tab -c c1 -t %2",
                &["move-pane -t %2 --to @1", "move-pane -t %2 --to @2"],
            ),
            (
                "choose-workspace -c c1",
                &["select-workspace -t +1", "select-workspace -t +2"],
            ),
            (
                "choose-workspace -c c1 --move",
                &["move-pane -t %2 --to +1", "move-pane -t %2 --to +2"],
            ),
            (
                "choose-pane -c c1 -t %2",
                &["swap-pane -t %2 %1", "swap-pane -t %2 %3"],
            ),
            ("confirm-close -c c1 pane -t %3", &["kill-pane -t %3"]),
            ("confirm-close -c c1 tab -t @2", &["kill-tab -t @2"]),
            (
                "confirm-close -c c1 workspace -t main",
                &["kill-workspace -t +1"],
            ),
        ] {
            run(&mut s, open)?;
            let expected = lines
                .iter()
                .map(|l| parsed(l))
                .collect::<Result<Vec<_>, _>>()?;
            assert_eq!(listed(&s), expected, "{open}");
        }
        // A chooser's `r` and `x` act on the selected item.
        run(&mut s, "choose-tab -c c1")?;
        s.input(c, b"r");
        assert_eq!(mode(&s, c), "prompt main|4");
        s.input(c, b"\r");
        run(&mut s, "choose-tab -c c1")?;
        s.input(c, b"x");
        assert_eq!(mode(&s, c), "confirm close tab @1 main?");
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
        // Home and End; Ctrl keys type and do nothing.
        s.input(c, b"\x1b[HY\x1b[FZ");
        assert_eq!(mode(&s, c), "prompt YaX界Z|5");
        s.input(c, b"\x01\x05\x15\x03\x07");
        assert_eq!(mode(&s, c), "prompt YaX界Z|5");
        s.input(c, b"\x7f\x7f\x7f\x7f\x7f");
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
