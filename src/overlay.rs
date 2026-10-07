//! The keyboard overlays: the command column, choosers, action menus, the
//! command prompt, rename prompts and confirmations. Each belongs to the client
//! that opened it.
use crate::command::{
    AnyRef, ClientAction, ClientId, Command, Kind, MoveTo, Pick, Sibling, SwapWith, WsRef,
};
use crate::config::Binding;
use crate::keys::{Direction, Key, KeyPress, Keystroke};
use crate::layout::{Node, PaneId};
use crate::session::{Ctx, Error, Session, describe};
use crate::view::{Confirm, Item, List, Mode, Prompt, PromptFor};

/// One row of the command column: a group heading, a binding, or a layer,
/// borrowed from the configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColumnRow<'a> {
    Heading(&'a str),
    Binding {
        key: KeyPress,
        label: String,
        command: &'a Command,
    },
    /// A key that opens a layer, and the layer's title.
    Layer {
        key: KeyPress,
        title: &'a str,
    },
}

/// An entry of the command column: a binding of its layer, a key that
/// opens a layer inside it, with the layer's first binding, or, right after
/// the prefix, a binding without the prefix (`bind -n`).
#[derive(Clone, Copy)]
enum Entry<'a> {
    Binding(KeyPress, &'a Binding),
    Layer(KeyPress, &'a Binding),
    Root(KeyPress, &'a Binding),
}

/// The group of bindings without the prefix that `-g` gave none.
pub const ROOT_GROUP: &str = "Without the prefix";

impl<'a> Entry<'a> {
    /// The group it is listed under: a binding's own, or the group of the
    /// command that a layer's first binding runs.
    fn group(self) -> &'a str {
        match self {
            Entry::Binding(_, binding) => binding.group(),
            Entry::Layer(_, first) => first.derived_group(),
            Entry::Root(_, binding) => binding.group.as_deref().unwrap_or(ROOT_GROUP),
        }
    }

    fn row(self) -> ColumnRow<'a> {
        match self {
            Entry::Binding(key, binding) | Entry::Root(key, binding) => ColumnRow::Binding {
                key,
                label: crate::command::label(&binding.command),
                command: &binding.parsed,
            },
            Entry::Layer(key, first) => ColumnRow::Layer {
                key,
                title: first.group(),
            },
        }
    }
}

/// The column's entries for the layer at `path`, in the order of the
/// bindings, found without building their rows. A layer is an entry once,
/// where its first binding is. Right after the prefix, the bindings without
/// it follow.
fn entries<'a>(session: &'a Session, path: &[KeyPress]) -> impl Iterator<Item = Entry<'a>> {
    let bindings = &session.config.bindings;
    let root = session.config.root.iter().filter(|_| path.is_empty());
    bindings
        .iter()
        .enumerate()
        .filter_map(move |(i, binding)| match binding.keys.strip_prefix(path) {
            Some([key]) => Some(Entry::Binding(*key, binding)),
            Some([key, _, ..]) if !bindings.iter().take(i).any(|b| opens(b, path, key)) => {
                Some(Entry::Layer(*key, binding))
            }
            Some(_) | None => None,
        })
        .chain(root.filter_map(|b| b.keys.first().map(|key| Entry::Root(*key, b))))
}

/// Whether `binding` is in the layer that `key` opens in the layer at `path`.
fn opens(binding: &Binding, path: &[KeyPress], key: &KeyPress) -> bool {
    matches!(binding.keys.strip_prefix(path), Some([k, _, ..]) if k == key)
}

/// The column's entries in its order, with their groups: groups in their
/// order, custom groups after them and `Other` last; then the bindings
/// without the prefix, in their own groups, so that none shares a heading
/// with keys typed after the prefix.
fn ordered<'a>(session: &'a Session, path: &[KeyPress]) -> Vec<(&'a str, Entry<'a>)> {
    let (root, entries): (Vec<_>, Vec<_>) = entries(session, path)
        .map(|e| (e.group(), e))
        .partition(|(_, e)| matches!(e, Entry::Root(..)));
    let mut groups: Vec<&str> = crate::config::GROUPS.to_vec();
    for (group, _) in &entries {
        if !groups.contains(group) && *group != "Other" {
            groups.push(group);
        }
    }
    groups.push("Other");
    let mut root_groups: Vec<&str> = Vec::new();
    for (group, _) in &root {
        if !root_groups.contains(group) {
            root_groups.push(group);
        }
    }
    let grouped = |groups: Vec<&'a str>, entries: Vec<(&'a str, Entry<'a>)>| {
        groups
            .into_iter()
            .flat_map(move |group| {
                entries
                    .iter()
                    .filter(|(g, _)| *g == group)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    let mut ordered = grouped(groups, entries);
    ordered.extend(grouped(root_groups, root));
    ordered
}

/// The command column's rows for the layer at `path`: its bindings and the
/// layers inside it, grouped, groups in their order, custom groups after
/// them and `Other` last. A layer is listed once, where its first binding
/// is, under the group its command belongs to.
pub fn column_rows<'a>(session: &'a Session, path: &[KeyPress]) -> Vec<ColumnRow<'a>> {
    let mut rows = Vec::new();
    let mut heading = None;
    for (group, entry) in ordered(session, path) {
        if heading != Some(group) {
            rows.push(ColumnRow::Heading(group));
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
pub fn column_selected<'a>(
    session: &'a Session,
    path: &[KeyPress],
    selected: usize,
) -> Option<ColumnRow<'a>> {
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
) -> Result<String, Error> {
    let view = session.views.get_mut(&client).ok_or(Error::NoSuchClient)?;
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
) -> Result<String, Error> {
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
    let view = session.views.get_mut(&client).ok_or(Error::NoSuchClient)?;
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
pub fn open_menu(session: &mut Session, client: ClientId, target: AnyRef) -> Result<String, Error> {
    // What the menu is for, which every item names.
    let about = match target {
        AnyRef::Workspace(r) => AnyRef::Workspace(WsRef::Id(session.resolve_ws(&r)?)),
        other @ (AnyRef::Pane(_) | AnyRef::Tab(_)) => other,
    };
    let kind = about.kind();
    let name = session.name_of(&about);
    let title = format!("{} {} {name}", kind.name(), describe(&about));
    let target = Some(about.clone());
    let reorder = |toward| Command::Reorder {
        kind,
        target: target.clone(),
        toward,
    };
    let mut items = vec![
        item(
            "rename",
            ClientAction::RenamePrompt {
                kind,
                target: target.clone(),
            }
            .here(),
        ),
        item(
            "close",
            ClientAction::ConfirmClose {
                kind,
                target: target.clone(),
            }
            .here(),
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
                ClientAction::ChoosePane { target: Some(p) }.here(),
            ),
            item(
                "move to tab…",
                ClientAction::ChooseTab {
                    moving: Some(p),
                    moving_now: false,
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
                    moving: Some(p),
                    moving_now: false,
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
        item("reorder previous", reorder(Sibling::Previous)),
        item("reorder next", reorder(Sibling::Next)),
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
) -> Result<String, Error> {
    let selected = items.iter().position(|i| i.current).unwrap_or(0);
    let view = session.views.get_mut(&client).ok_or(Error::NoSuchClient)?;
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
) -> Result<String, Error> {
    let view = session.views.get(&client).ok_or(Error::NoSuchClient)?;
    let current = view.tab();
    let ws = session
        .workspace(view.workspace)
        .ok_or(Error::NoCurrentWorkspace)?;
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
                    None => ClientAction::SelectTab(Pick::Id(id)).here(),
                },
                current: Some(id) == current,
                subject: Some(AnyRef::Tab(id)),
            }
        })
        .collect();
    open_chooser(session, client, "tab", items, moving)
}

/// Every workspace, with its tabs' panes.
pub fn open_workspace_chooser(
    session: &mut Session,
    client: ClientId,
    moving: Option<PaneId>,
) -> Result<String, Error> {
    let current = session
        .views
        .get(&client)
        .ok_or(Error::NoSuchClient)?
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
                    None => ClientAction::SelectWorkspace(Pick::Id(WsRef::Id(ws.id))).here(),
                },
                current: ws.id == current,
                subject: Some(AnyRef::Workspace(WsRef::Id(ws.id))),
            }
        })
        .collect();
    open_chooser(session, client, "workspace", items, moving)
}

/// A chooser of tabs or workspaces, `what`, to select one or to move a pane
/// to.
fn open_chooser(
    session: &mut Session,
    client: ClientId,
    what: &str,
    items: Vec<Item>,
    moving: Option<PaneId>,
) -> Result<String, Error> {
    let title = match moving {
        Some(p) => format!("move {p} to {what}"),
        None => format!("{what}s"),
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
) -> Result<String, Error> {
    let (_, tab) = session.locate(source).ok_or(Error::NotInTab)?;
    let mut items: Vec<Item> = Vec::new();
    if let Some(root) = session.root(tab) {
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
        return Err(Error::OnlyOne(Kind::Pane));
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
    if outcome.status != 0 {
        let line = outcome.stderr.lines().next().unwrap_or("failed");
        session.error_to(client, line);
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
        session.info_to(
            client,
            if more {
                format!("{line} …")
            } else {
                line.to_owned()
            },
        );
    }
}

/// Runs a command line typed for a client, as `run_for` runs a command; one
/// that does not parse says why.
fn run_line(session: &mut Session, client: ClientId, argv: &[String]) {
    match crate::command::parse(argv) {
        Ok(command) => run_for(session, client, &command),
        Err(usage) => {
            let message = usage.to_string();
            session.error_to(client, message.lines().next().unwrap_or("failed"));
        }
    }
}

/// Runs an entry of a list or the column, unless it cannot run now, in
/// which case the reason is shown and nothing happens: a list closes, to
/// `closed`, only if its entry runs.
fn run_entry(session: &mut Session, client: ClientId, command: &Command, closed: Option<Mode>) {
    if let Some(reason) = session.unavailable(command, &Ctx::client(client)) {
        return session.error_to(client, reason.to_string());
    }
    if let Some(mode) = closed {
        session.set_mode(client, mode);
    }
    run_for(session, client, command);
}

/// Runs the binding of `keys`, if there is one, entering its layer's repeat
/// mode if it repeats: whether there is one.
fn run_binding(session: &mut Session, client: ClientId, keys: &[KeyPress]) -> bool {
    let Some(binding) = session.config.bindings.iter().find(|b| b.keys == keys) else {
        return false;
    };
    let command = binding.parsed.clone();
    let mode = match keys.split_last() {
        Some((_, path)) if binding.repeat => Mode::Repeat {
            path: path.to_vec(),
        },
        Some(_) | None => Mode::Normal,
    };
    session.set_mode(client, mode);
    run_entry(session, client, &command, None);
    true
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

/// A key while the command column is open.
pub fn column_key(session: &mut Session, client: ClientId, press: KeyPress) {
    let prefix = session.config.prefix;
    let Some(view) = session.views.get(&client) else {
        return;
    };
    let Mode::Column { path, selected } = &view.mode else {
        return;
    };
    let (path, selected, rows) = (path.clone(), *selected, view.rows);
    let len = column_len(session, &path);
    let (_, page) = column_room(rows);
    let last = len.saturating_sub(1);
    // Escape always closes the column, and cannot be bound. Any other key
    // bound in this layer, or opening a layer in it, is the binding's; the
    // column navigates with the keys left unbound.
    if press.key == Key::Escape && !press.mods.ctrl && !press.mods.alt {
        session.set_mode(client, Mode::Normal);
        return;
    }
    if binds(session, &path, press) {
        follow(session, client, &path, press);
        return;
    }
    let unmodified = press.mods.is_empty().then_some(press.key);
    let new = match unmodified {
        _ if press == prefix => {
            // The prefix, at any depth, sends it to the pane.
            session.set_mode(client, Mode::Normal);
            send_key(session, client, prefix.into());
            return;
        }
        Some(Key::Arrow(Direction::Up)) => selected.saturating_sub(1),
        // Moves stop at the first and last entries.
        Some(Key::Arrow(Direction::Down)) => selected.saturating_add(1).min(last),
        Some(Key::PageUp) => selected.saturating_sub(page),
        Some(Key::PageDown) => selected.saturating_add(page).min(last),
        Some(Key::Home) => 0,
        Some(Key::End) => last,
        Some(Key::Enter) => {
            match column_selected(session, &path, selected) {
                Some(ColumnRow::Binding { command, .. }) => {
                    let command = command.clone();
                    session.set_mode(client, Mode::Normal);
                    run_entry(session, client, &command, None);
                }
                Some(ColumnRow::Layer { key, .. }) => follow(session, client, &path, key),
                Some(ColumnRow::Heading(_)) | None => session.set_mode(client, Mode::Normal),
            }
            return;
        }
        _ => {
            follow(session, client, &path, press);
            return;
        }
    };
    session.set_mode(
        client,
        Mode::Column {
            path,
            selected: new,
        },
    );
}

/// Whether `press`, typed in the layer at `path`, is bound there or opens a
/// layer inside it. Keys match as typed: `V` is not `v`.
fn binds(session: &Session, path: &[KeyPress], press: KeyPress) -> bool {
    session.config.bindings.iter().any(|b| {
        b.keys.len() > path.len()
            && b.keys.starts_with(path)
            && b.keys.get(path.len()) == Some(&press)
    })
}

/// A key typed in the layer at `path`: it runs its binding, entering the
/// layer's repeat mode if the binding repeats; opens the layer it starts;
/// or, unbound, leaves the column open, saying so.
fn follow(session: &mut Session, client: ClientId, path: &[KeyPress], press: KeyPress) {
    let mut keys = path.to_vec();
    keys.push(press);
    if run_binding(session, client, &keys) {
        return;
    }
    if session.is_layer(&keys) {
        session.set_mode(
            client,
            Mode::Column {
                path: keys,
                selected: 0,
            },
        );
    } else {
        let prefix = session.config.prefix;
        let keys = crate::config::keys_text(&keys);
        session.error_to(client, format!("{prefix} {keys} is not bound"));
    }
}

/// A key in a repeat mode: one of its layer's keys runs its binding again,
/// without the prefix; Esc or Enter leaves; the prefix leaves and opens the
/// column; any other key leaves, not reaching the pane, and says so.
pub fn repeat_key(session: &mut Session, client: ClientId, press: KeyPress) {
    let prefix = session.config.prefix;
    let Some(Mode::Repeat { path }) = session.views.get(&client).map(|v| &v.mode) else {
        return;
    };
    let path = path.clone();
    if press == prefix {
        session.set_mode(
            client,
            Mode::Column {
                path: Vec::new(),
                selected: 0,
            },
        );
        return;
    }
    // Escape leaves; so does Enter, unless the layer binds it.
    let plain = !press.mods.ctrl && !press.mods.alt;
    if plain
        && (press.key == Key::Escape || (press.key == Key::Enter && !binds(session, &path, press)))
    {
        session.set_mode(client, Mode::Normal);
        return;
    }
    let mut keys = path.clone();
    keys.push(press);
    if !run_binding(session, client, &keys) {
        let title = layer_title(session, &path).unwrap_or_default().to_owned();
        session.set_mode(client, Mode::Normal);
        session.info_to(
            client,
            format!("{title} ended: {press} is not one of its keys"),
        );
    }
}

/// Runs the binding without the prefix of `press`, if there is one, and
/// says whether there was.
pub fn run_root(session: &mut Session, client: ClientId, press: KeyPress) -> bool {
    let Some(binding) = session.config.root.iter().find(|b| b.keys == [press]) else {
        return false;
    };
    let command = binding.parsed.clone();
    run_entry(session, client, &command, None);
    true
}

/// Sends a key to the client's focused pane, encoded as its program asked.
pub fn send_key(session: &mut Session, client: ClientId, stroke: Keystroke) {
    let Some(pane) = session.views.get(&client).and_then(|v| v.focus()) else {
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
    let page = list_room(rows);
    let mut run: Option<Command> = None;
    let mut close = false;
    match press.plain_key() {
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
        Some(Key::Enter) => run = list.items.get(list.selected).map(|i| i.command.clone()),
        Some(Key::Char(key @ ('r' | 'x'))) if list.chooser => {
            if let Some(subject) = list
                .items
                .get(list.selected)
                .and_then(|i| i.subject.clone())
            {
                let (kind, target) = (subject.kind(), Some(subject));
                let action = if key == 'r' {
                    ClientAction::RenamePrompt { kind, target }
                } else {
                    ClientAction::ConfirmClose { kind, target }
                };
                run = Some(action.here());
            }
        }
        _ => {}
    }
    view.dirty = true;
    match run {
        // An unavailable entry explains itself and the list stays open.
        Some(command) => run_entry(session, client, &command, Some(Mode::Normal)),
        None if close => session.set_mode(client, Mode::Normal),
        None => {}
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
                Err(error) => return session.error_to(client, error.to_string()),
            };
            run_line(session, client, &argv);
        }
        PromptFor::Rename(target) => {
            // The command is built, not parsed from words: a name is the
            // text as typed, `-dev` and `--` included. A workspace is held
            // by its ID, in case its name changed while the prompt was open.
            let target = match target {
                AnyRef::Workspace(r) => match session.resolve_ws(&r) {
                    Ok(w) => AnyRef::Workspace(WsRef::Id(w)),
                    Err(error) => return session.error_to(client, error.to_string()),
                },
                other @ (AnyRef::Pane(_) | AnyRef::Tab(_)) => other,
            };
            run_for(
                session,
                client,
                &Command::Rename {
                    target,
                    name: prompt.text,
                },
            );
        }
    }
}

/// A key while a confirmation waits: `y` runs it, `n`, `q` or Escape
/// cancels.
pub fn confirm_key(session: &mut Session, client: ClientId, press: KeyPress) {
    let Some(view) = session.views.get_mut(&client) else {
        return;
    };
    let Mode::Confirm(confirm) = &view.mode else {
        return;
    };
    view.dirty = true;
    match press.plain_key() {
        Some(Key::Char('y')) => {
            let command = confirm.command.clone();
            view.mode = Mode::Normal;
            run_for(session, client, &command);
        }
        Some(Key::Char('n')) | Some(Key::Escape) | Some(Key::Char('q')) => {
            view.mode = Mode::Normal;
        }
        _ => {}
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
    use crate::session::testing::run;

    fn session() -> Result<(Session, ClientId), String> {
        crate::session::testing::attached(30, 100)
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

    /// A name typed into a rename prompt is the name, whatever it looks
    /// like: one that starts with `-` is not taken for a flag. (A
    /// workspace's may not start with one, as no target could read it
    /// back: its flag-like word comes after.)
    #[test]
    fn a_rename_prompt_takes_any_name() -> Outcome {
        let (mut session, client) = session()?;
        for (open, name, read) in [
            ("rename-prompt -c c1 pane -t %1", "-dev", "%1"),
            ("rename-prompt -c c1 tab -t @1", "--", "@1"),
            ("rename-prompt -c c1 workspace -t +1", "x -w --", "+1"),
        ] {
            run(&mut session, open)?;
            // Backspace clears what the prompt starts with, the current name.
            session.input(client, &[0x7f; 16]);
            session.input(client, name.as_bytes());
            session.input(client, b"\r");
            assert_eq!(notice(&session, client), "", "{open}");
            let target = crate::command::parse_any(read).map_err(|e| e.to_string())?;
            assert_eq!(session.name_of(&target), name, "{open}");
        }
        Ok(())
    }

    /// Prompt edits count chars, so none falls inside one: what Backspace,
    /// Delete, typing and a paste do, on text with wide chars.
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

    /// Like the keys after the prefix and copy mode's, a chooser's and a
    /// confirmation's letter keys work in either case: Caps Lock changes
    /// nothing.
    /// On a screen too short for a list's lines, the selected entry stays
    /// in view: the lines around it give way first.
    #[test]
    fn a_short_screen_keeps_the_selected_entry_in_view() -> Outcome {
        let (mut s, c) = crate::session::testing::attached(3, 40)?;
        for _ in 0..3 {
            run(&mut s, "new-tab -t +1")?;
        }
        run(&mut s, "choose-tab -c c1")?;
        s.input(c, b"\x1b[H");
        let shown = screen_text(&s, c)?;
        assert!(shown.contains("@1 main"), "{shown}");
        Ok(())
    }

    #[test]
    fn list_and_confirm_keys_ignore_case() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "new-tab -t +1")?;
        run(&mut s, "choose-tab -c c1")?;
        s.input(c, b"\x1b[H");
        s.input(c, b"J");
        assert!(mode(&s, c).ends_with(" 1"), "J moves down: {}", mode(&s, c));
        s.input(c, b"K");
        assert!(mode(&s, c).ends_with(" 0"), "K moves up: {}", mode(&s, c));
        s.input(c, b"Q");
        assert_eq!(mode(&s, c), "normal", "Q closes");
        run(&mut s, "confirm-close -c c1 tab -t @2")?;
        s.input(c, b"Q");
        assert_eq!(mode(&s, c), "normal", "Q cancels");
        run(&mut s, "confirm-close -c c1 tab -t @2")?;
        s.input(c, b"Y");
        assert!(
            !s.exists(&AnyRef::Tab(crate::command::TabId(2))),
            "Y confirms"
        );
        Ok(())
    }

    /// A long chooser scrolled to the middle shows its title, both "more"
    /// lines, its items and its help, all within the screen (41 tabs, the
    /// cursor at the 31st, on 30 rows lost the title).
    #[test]
    fn a_scrolled_list_keeps_its_title() -> Outcome {
        let (mut s, c) = session()?;
        for _ in 0..40 {
            run(&mut s, "new-tab -t +1")?;
        }
        run(&mut s, "choose-tab -c c1")?;
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
        let page = column_room(30).1.min(last);
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

    fn pane_width(s: &Session, pane: u32) -> u16 {
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
            title: "Tabs",
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
                ColumnRow::Heading("Tabs"),
                ColumnRow::Binding {
                    key: KeyPress::char('n'),
                    label: crate::command::label(&["new-tab".to_owned()]),
                    command: &Command::NewTab {
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
        let start = pane_width(&s, 1);
        s.input(c, b"\x02y");
        assert_eq!(mode(&s, c), "column y 0");
        s.input(c, b"l");
        assert_eq!(mode(&s, c), "repeat y");
        // No prefix: the mode's keys run again and again.
        s.input(c, b"ll");
        assert_eq!(start.checked_add(3), Some(pane_width(&s, 1)));
        s.input(c, b"h");
        assert_eq!(start.checked_add(2), Some(pane_width(&s, 1)));
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
        assert_eq!(start.checked_add(3), Some(pane_width(&s, 1)));
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
        // What it was, kept past the output that changes the session.
        let selected = column_selected(&s, &[], 3).map(|row| format!("{row:?}"));
        assert!(selected.is_some());
        for i in 0..50 {
            s.output(
                crate::layout::PaneId(1),
                format!("output {i}\r\n").as_bytes(),
            );
            s.settle();
        }
        assert_eq!(mode(&s, c), "column 3");
        let now = column_selected(&s, &[], 3).map(|row| format!("{row:?}"));
        assert_eq!(now, selected);
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
            run_entry(&mut ran, d, &binding.parsed, None);
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
            crate::command::parse(&crate::words::split(line).map_err(|e| e.to_string())?)
                .map_err(|u| u.to_string())
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

    /// How many tabs the first workspace has.
    fn tabs(s: &Session) -> Option<usize> {
        s.workspaces.first().map(|w| w.tabs.len())
    }

    #[test]
    fn keys_after_the_prefix_match_as_typed() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "bind V new-tab")?;
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
        run(&mut s, "bind Up new-tab")?;
        s.input(c, &prefixed("\x1b[A"));
        assert_eq!(tabs(&s), Some(2));
        assert_eq!(mode(&s, c), "normal");
        // Down, still unbound, still moves.
        s.input(c, &prefixed("\x1b[B"));
        assert_eq!(mode(&s, c), "column 1");
        escape(&mut s, c);
        // The prefix twice sends it, until the layer binds it.
        let pane = s.views.get(&c).and_then(|v| v.focus()).map_or(0, |p| p.0);
        let _ = queued(&mut s, pane);
        s.input(c, &[0x02, 0x02]);
        assert_eq!(queued(&mut s, pane), b"\x02");
        run(&mut s, "bind C-b new-tab")?;
        s.input(c, &[0x02, 0x02]);
        assert_eq!(tabs(&s), Some(3));
        let pane = s.views.get(&c).and_then(|v| v.focus()).map_or(0, |p| p.0);
        assert!(queued(&mut s, pane).is_empty());
        // `send-prefix` sends it from anywhere.
        run(&mut s, &format!("send-prefix -t %{pane}"))?;
        assert_eq!(queued(&mut s, pane), b"\x02");
        // A digit, a function key and a chord bind as letters do.
        run(&mut s, "bind 1 new-tab")?;
        run(&mut s, "bind F5 new-tab")?;
        run(&mut s, "bind M-n new-tab")?;
        s.input(c, &prefixed("1"));
        s.input(c, &prefixed("\x1b[15~"));
        s.input(c, &prefixed("\x1bn"));
        assert_eq!(tabs(&s), Some(6));
        Ok(())
    }

    #[test]
    fn a_repeat_mode_may_bind_enter() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "split -h -t %1")?;
        run(&mut s, "select-pane -c c1 -t %1")?;
        let start = pane_width(&s, 1);
        run(&mut s, "bind -r r Enter resize-pane -R")?;
        s.input(c, &prefixed("r\r\r"));
        assert_eq!(mode(&s, c), "repeat r");
        assert_eq!(start.checked_add(2), Some(pane_width(&s, 1)));
        escape(&mut s, c);
        assert_eq!(mode(&s, c), "normal");
        Ok(())
    }

    #[test]
    fn a_key_bound_without_the_prefix_runs_at_once_and_reaches_no_pane() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "bind -n M-t new-tab")?;
        s.input(c, b"\x1bt");
        assert_eq!(tabs(&s), Some(2));
        let pane = s.views.get(&c).and_then(|v| v.focus()).map_or(0, |p| p.0);
        assert!(queued(&mut s, pane).is_empty());
        // A chord bound to nothing reaches the pane as it was typed.
        s.input(c, b"\x1bl");
        assert_eq!(queued(&mut s, pane), b"\x1bl");
        // In copy mode and in the column the key is theirs, not the binding's.
        run(&mut s, "copy-mode -c c1")?;
        s.input(c, b"\x1bt");
        assert_eq!(tabs(&s), Some(2));
        s.input(c, b"q");
        assert_eq!(mode(&s, c), "normal");
        s.input(c, &prefixed("\x1bt"));
        assert_eq!(tabs(&s), Some(2));
        assert_eq!(notice(&s, c), "C-b M-t is not bound");
        escape(&mut s, c);
        // The column lists it last, under its own heading, and runs it.
        let rows = column_rows(&s, &[]);
        let at = rows
            .iter()
            .position(|r| *r == ColumnRow::Heading(ROOT_GROUP));
        assert!(
            at.is_some_and(|at| at.checked_add(2) == Some(rows.len())),
            "{rows:?}"
        );
        let last = column_len(&s, &[]).saturating_sub(1);
        s.input(c, &prefixed(""));
        for _ in 0..last {
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
        placement.rect(crate::layout::PaneId(pane))
    }

    /// The mouse tracking client `c`'s terminal is asked for.
    fn asked_mouse_level(s: &Session, c: ClientId) -> Option<u16> {
        crate::render::compose(s, c).map(|g| g.mouse)
    }

    #[test]
    fn a_mouse_report_reaches_the_focused_pane_in_its_cells() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "split -h -t %1")?;
        run(&mut s, "select-pane -c c1 -t %2")?;
        let right = rect_of(&s, c, 2).ok_or("no rect for %2")?;
        assert!(right.x > 1 && right.y == 0, "{right:?}");
        // Nothing asked: no reporting, and a report is dropped.
        assert_eq!(asked_mouse_level(&s, c), Some(0));
        let press = |col: u16, row: u16| format!("\x1b[<0;{};{}M", col + 1, row + 1);
        s.input(c, press(right.x + 3, 2).as_bytes());
        assert!(queued(&mut s, 2).is_empty());
        s.output(crate::layout::PaneId(2), b"\x1b[?1000h\x1b[?1006h");
        assert_eq!(asked_mouse_level(&s, c), Some(1000));
        // Moved to the pane's cells.
        s.input(c, press(right.x + 3, 2).as_bytes());
        assert_eq!(queued(&mut s, 2), b"\x1b[<0;4;3M");
        // On the other pane, or the border between them: dropped.
        s.input(c, press(1, 2).as_bytes());
        s.input(c, press(right.x - 1, 2).as_bytes());
        assert!(queued(&mut s, 2).is_empty() && queued(&mut s, 1).is_empty());
        // In the encoding the program asked for: here the default.
        s.output(crate::layout::PaneId(2), b"\x1b[?1006l");
        s.input(c, press(right.x, 0).as_bytes());
        assert_eq!(queued(&mut s, 2), [0x1b, b'[', b'M', 32, 33, 33]);
        // X10's presses come from asking for 1000, its releases dropped.
        s.output(crate::layout::PaneId(2), b"\x1b[?9h\x1b[?1006h");
        assert_eq!(asked_mouse_level(&s, c), Some(1000));
        s.input(
            c,
            format!("{}\x1b[<0;{};1m", press(right.x, 0), right.x + 1).as_bytes(),
        );
        assert_eq!(queued(&mut s, 2), b"\x1b[<0;1;1M");
        // A pane below another: moved down too.
        run(&mut s, "split -v -t %2")?;
        run(&mut s, "select-pane -c c1 -t %3")?;
        let below = rect_of(&s, c, 3).ok_or("no rect for %3")?;
        assert!(below.y > 1, "{below:?}");
        s.output(crate::layout::PaneId(3), b"\x1b[?1000h\x1b[?1006h");
        s.input(c, press(below.x + 1, below.y + 2).as_bytes());
        assert_eq!(queued(&mut s, 3), b"\x1b[<0;2;3M");
        Ok(())
    }

    #[test]
    fn a_drag_out_of_the_pane_is_kept_to_its_edge() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "split -h -t %1")?;
        run(&mut s, "select-pane -c c1 -t %2")?;
        let right = rect_of(&s, c, 2).ok_or("no rect for %2")?;
        s.output(crate::layout::PaneId(2), b"\x1b[?1002h\x1b[?1006h");
        assert_eq!(asked_mouse_level(&s, c), Some(1002));
        let x = right.x + 1;
        // Pressed inside, dragged over the left pane and past the bottom,
        // released there: the pane hears all of it, at its edge.
        s.input(c, format!("\x1b[<0;{};2M", x + 1).as_bytes());
        s.input(c, b"\x1b[<32;1;2M");
        s.input(c, b"\x1b[<32;1;200M");
        s.input(c, b"\x1b[<0;1;200m");
        let bottom = right.h;
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
        run(&mut s, "split -h -t %1")?;
        run(&mut s, "select-pane -c c1 -t %2")?;
        s.output(crate::layout::PaneId(2), b"\x1b[?1003h\x1b[?1006h");
        assert_eq!(asked_mouse_level(&s, c), Some(1003));
        let right = rect_of(&s, c, 2).ok_or("no rect for %2")?;
        let press = format!("\x1b[<0;{};2M", right.x + 2);
        // Copy mode, the column and a prompt have the keys: no reporting,
        // and a report in flight is dropped.
        for (open, close) in [
            ("copy-mode -c c1", &b"q"[..]),
            ("command-column -c c1", b"\x1b"),
            ("command-prompt -c c1", b"\x1b"),
        ] {
            run(&mut s, open)?;
            assert_eq!(asked_mouse_level(&s, c), Some(0), "{open}");
            s.input(c, press.as_bytes());
            assert!(queued(&mut s, 2).is_empty(), "{open}");
            s.input(c, close);
            std::thread::sleep(crate::decode::ESCAPE_DELAY);
            s.escape(c);
            assert_eq!(mode(&s, c), "normal", "{open}");
            assert_eq!(asked_mouse_level(&s, c), Some(1003), "{open}");
        }
        // Focus on a pane whose program did not ask: none.
        run(&mut s, "select-pane -c c1 -t %1")?;
        assert_eq!(asked_mouse_level(&s, c), Some(0));
        s.input(c, b"\x1b[<0;2;2M");
        assert!(queued(&mut s, 1).is_empty() && queued(&mut s, 2).is_empty());
        // Zoomed, the pane fills the screen, and its cells are the screen's.
        run(&mut s, "select-pane -c c1 -t %2")?;
        run(&mut s, "zoom -c c1")?;
        s.input(c, b"\x1b[<0;2;2M");
        assert_eq!(queued(&mut s, 2), b"\x1b[<0;2;2M");
        Ok(())
    }

    #[test]
    fn resize_mode_repeats_its_keys_until_esc() -> Outcome {
        let (mut s, c) = session()?;
        run(&mut s, "split -h -t %1")?;
        run(&mut s, "select-pane -c c1 -t %1")?;
        let start = pane_width(&s, 1);
        s.input(c, &prefixed("rlll"));
        assert_eq!(mode(&s, c), "repeat r");
        assert_eq!(start.checked_add(3), Some(pane_width(&s, 1)));
        escape(&mut s, c);
        assert_eq!(mode(&s, c), "normal");
        // After Esc, `l` is the pane's again.
        let _ = queued(&mut s, 1);
        s.input(c, b"l");
        assert_eq!(queued(&mut s, 1), b"l");
        assert_eq!(start.checked_add(3), Some(pane_width(&s, 1)));
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
        escape(&mut s, c);
        assert_eq!(mode(&s, c), "normal");
        Ok(())
    }
}
