//! The server's state: workspaces, tabs, panes and the clients' views, and
//! every command that changes them.
use crate::command::{
    self, AnyRef, ClientId, Command, Kind, MoveTo, Pick, SwapWith, TabId, Usage, WsId, WsRef,
};
use crate::config::Config;
use crate::json::Json;
use crate::keys::{Direction, KeyPress};
use crate::layout::{self, Axis, Node, PaneId, Placement, Rect};
use crate::pane::Pane;
use crate::process::Pid;
use crate::view::{Mode, View};
use std::collections::{BTreeMap, VecDeque};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct Tab {
    pub id: TabId,
    pub name: String,
    pub root: Option<Node>,
}

pub struct Workspace {
    pub id: WsId,
    pub name: String,
    pub tabs: Vec<Tab>,
}

/// What the server must do outside the session.
#[derive(Debug, PartialEq, Eq)]
pub enum Outgoing {
    /// Bytes for a client's terminal outside any paint (OSC 52).
    Bytes(ClientId, Vec<u8>),
    /// Detach a client, telling it why.
    Exit(ClientId, String),
    /// Stop the server.
    Shutdown(String),
}

/// A process being ended: hung up, and killed and reaped at the deadline.
pub struct Dying {
    pub pid: Pid,
    /// Closed before the group is killed.
    pub master: Option<OwnedFd>,
    pub deadline: Instant,
}

/// Where a command comes from.
#[derive(Clone, Debug, Default)]
pub struct Ctx {
    /// The client whose key, prompt or menu ran it.
    pub client: Option<ClientId>,
    /// `FUX_PANE` of the CLI's caller.
    pub pane: Option<PaneId>,
    /// The CLI caller's working directory.
    pub cwd: Option<PathBuf>,
}

impl Ctx {
    pub fn client(id: ClientId) -> Ctx {
        Ctx {
            client: Some(id),
            ..Ctx::default()
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub status: u8,
    pub stdout: String,
    pub stderr: String,
}

/// How long a closing pane's processes have to exit after the hangup.
pub const GRACE: Duration = Duration::from_millis(100);
/// The longest a typed command waits for the new shell to write and then
/// go quiet (`pane::QUIET`): a shell that writes nothing still gets it.
pub const TYPE_WAIT: Duration = Duration::from_secs(1);
/// The size a pane gets when no client shows it yet.
const DEFAULT_SIZE: (u16, u16) = (24, 80);

pub struct Session {
    pub workspaces: Vec<Workspace>,
    pub panes: BTreeMap<PaneId, Pane>,
    pub views: BTreeMap<ClientId, View>,
    pub config: Config,
    pub config_path: Option<PathBuf>,
    /// Why the config file could not be used, until a reload succeeds.
    pub config_error: Option<String>,
    /// Paste buffers, newest first.
    pub buffers: VecDeque<String>,
    pub socket: PathBuf,
    /// Start real processes; tests of the state alone do not.
    pub launch: bool,
    pub outbox: Vec<Outgoing>,
    pub dying: Vec<Dying>,
    next_pane: u32,
    next_tab: u32,
    next_ws: u32,
    next_client: u32,
}

fn basename(program: &str) -> String {
    Path::new(program)
        .file_name()
        .map_or_else(|| program.to_owned(), |n| n.to_string_lossy().into_owned())
}

impl Session {
    pub fn new(config: Config, socket: PathBuf, launch: bool) -> Session {
        Session {
            workspaces: Vec::new(),
            panes: BTreeMap::new(),
            views: BTreeMap::new(),
            config,
            config_path: None,
            config_error: None,
            buffers: VecDeque::new(),
            socket,
            launch,
            outbox: Vec::new(),
            dying: Vec::new(),
            next_pane: 1,
            next_tab: 1,
            next_ws: 1,
            next_client: 1,
        }
    }

    /// One workspace, holding one tab with one shell.
    pub fn start(&mut self) -> Result<(), String> {
        self.create_workspace(Some("main".into()), &[], None)
            .map(|_| ())
    }

    // ---------------------------------------------------------------- lookup

    pub fn ws_index(&self, id: WsId) -> Option<usize> {
        self.workspaces.iter().position(|w| w.id == id)
    }
    pub fn workspace(&self, id: WsId) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.id == id)
    }
    /// The workspace and tab indexes of a tab.
    pub fn find_tab(&self, id: TabId) -> Option<(usize, usize)> {
        self.workspaces
            .iter()
            .enumerate()
            .find_map(|(w, ws)| ws.tabs.iter().position(|t| t.id == id).map(|t| (w, t)))
    }
    pub fn tab(&self, id: TabId) -> Option<&Tab> {
        let (w, t) = self.find_tab(id)?;
        self.workspaces.get(w)?.tabs.get(t)
    }
    fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        let (w, t) = self.find_tab(id)?;
        self.workspaces.get_mut(w)?.tabs.get_mut(t)
    }
    /// The workspace and tab that hold a pane.
    pub fn locate(&self, pane: PaneId) -> Option<(WsId, TabId)> {
        self.workspaces.iter().find_map(|ws| {
            ws.tabs
                .iter()
                .find(|t| t.root.as_ref().is_some_and(|r| r.contains(pane)))
                .map(|t| (ws.id, t.id))
        })
    }
    fn tab_workspace(&self, tab: TabId) -> Option<WsId> {
        let (w, _) = self.find_tab(tab)?;
        self.workspaces.get(w).map(|ws| ws.id)
    }
    pub fn resolve_ws(&self, r: &WsRef) -> Result<WsId, String> {
        match r {
            WsRef::Id(id) => self
                .workspace(*id)
                .map(|w| w.id)
                .ok_or_else(|| format!("no workspace {id}")),
            WsRef::Name(name) => self
                .workspaces
                .iter()
                .find(|w| &w.name == name)
                .map(|w| w.id)
                .ok_or_else(|| format!("no workspace named {name:?}")),
        }
    }
    pub fn tab_panes(&self, tab: TabId) -> Vec<PaneId> {
        self.tab(tab)
            .and_then(|t| t.root.as_ref())
            .map(Node::panes)
            .unwrap_or_default()
    }
    pub fn exists(&self, what: &AnyRef) -> bool {
        match what {
            AnyRef::Pane(p) => self.panes.contains_key(p),
            AnyRef::Tab(t) => self.find_tab(*t).is_some(),
            AnyRef::Workspace(w) => self.resolve_ws(w).is_ok(),
        }
    }

    // ------------------------------------------------------------- targets

    fn view_of(&self, ctx: &Ctx) -> Option<&View> {
        ctx.client.and_then(|c| self.views.get(&c))
    }

    fn pane_target(&self, explicit: Option<PaneId>, ctx: &Ctx) -> Result<PaneId, String> {
        let id = explicit
            .or_else(|| self.view_of(ctx).and_then(View::focus))
            .or(ctx.pane)
            .ok_or("no pane given: use -t %N")?;
        if self.panes.contains_key(&id) {
            Ok(id)
        } else {
            Err(format!("no pane {id}"))
        }
    }

    fn tab_target(&self, explicit: Option<TabId>, ctx: &Ctx) -> Result<TabId, String> {
        let id = explicit
            .or_else(|| self.view_of(ctx).and_then(View::tab))
            .or_else(|| ctx.pane.and_then(|p| self.locate(p)).map(|(_, t)| t))
            .ok_or("no tab given: use -t @N")?;
        self.find_tab(id)
            .map(|_| id)
            .ok_or_else(|| format!("no tab {id}"))
    }

    fn ws_target(&self, explicit: Option<&WsRef>, ctx: &Ctx) -> Result<WsId, String> {
        if let Some(r) = explicit {
            return self.resolve_ws(r);
        }
        self.view_of(ctx)
            .map(|v| v.workspace)
            .or_else(|| ctx.pane.and_then(|p| self.locate(p)).map(|(w, _)| w))
            .ok_or_else(|| "no workspace given: use -t +N or a name".to_owned())
    }

    fn any_target(
        &self,
        kind: Kind,
        explicit: Option<&AnyRef>,
        ctx: &Ctx,
    ) -> Result<AnyRef, String> {
        if let Some(target) = explicit {
            return if self.exists(target) {
                Ok(target.clone())
            } else {
                Err(format!("no {} {}", kind.name(), describe(target)))
            };
        }
        Ok(match kind {
            Kind::Pane => AnyRef::Pane(self.pane_target(None, ctx)?),
            Kind::Tab => AnyRef::Tab(self.tab_target(None, ctx)?),
            Kind::Workspace => AnyRef::Workspace(WsRef::Id(self.ws_target(None, ctx)?)),
        })
    }

    fn client_target(&self, explicit: Option<ClientId>, ctx: &Ctx) -> Result<ClientId, String> {
        let id = explicit.or(ctx.client).ok_or(
            "this command acts on a client's screen: use -c CLIENT (`fux ls` lists clients)",
        )?;
        if self.views.contains_key(&id) {
            Ok(id)
        } else {
            Err(format!("no client {id}"))
        }
    }

    fn view_mut(&mut self, id: ClientId) -> Result<&mut View, String> {
        self.views
            .get_mut(&id)
            .ok_or_else(|| format!("no client {id}"))
    }

    // ------------------------------------------------------------ creation

    fn cwd_for(&self, ctx: &Ctx, near: Option<PaneId>) -> PathBuf {
        if let Some(cwd) = &ctx.cwd {
            return cwd.clone();
        }
        let near = near.or_else(|| self.view_of(ctx).and_then(View::focus));
        if let Some(cwd) = near
            .and_then(|p| self.panes.get(&p))
            .and_then(|p| p.child.as_ref())
            .and_then(|c| crate::process::cwd(c.pid))
        {
            return cwd;
        }
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    /// A new pane running the shell, with `cmd` typed into it if given.
    fn new_pane(&mut self, cmd: &[String], cwd: &Path, size: (u16, u16)) -> Result<PaneId, String> {
        let shell_program = self
            .config
            .shell
            .first()
            .cloned()
            .unwrap_or_else(|| "/bin/sh".into());
        let fish = basename(&shell_program) == "fish";
        let line = if cmd.is_empty() {
            None
        } else {
            Some(crate::words::shell_line(cmd, fish)?)
        };
        let id = PaneId(self.next_pane);
        let mut next_pane = self.next_pane;
        advance(&mut next_pane, "pane")?;
        let name = cmd
            .first()
            .map(|c| basename(c))
            .unwrap_or_else(|| basename(&shell_program));
        let mut pane = Pane::new(
            id,
            name,
            shell_program,
            size.0,
            size.1,
            self.config.history_lines,
        )?;
        if self.launch {
            let env = [
                ("FUX_PANE", id.to_string()),
                ("FUX_SOCKET", self.socket.to_string_lossy().into_owned()),
            ];
            pane.child = Some(crate::process::spawn(
                &self.config.shell,
                cwd,
                &env,
                size.0,
                size.1,
            )?);
        }
        if let Some(line) = line {
            let mut typed = line.into_bytes();
            typed.push(b'\r');
            if typed
                .len()
                .checked_add(crate::pane::ENTRY_COST)
                .is_none_or(|cost| cost > crate::pane::INPUT_BYTES)
            {
                return Err("the command line is too long to type".into());
            }
            pane.typed = Some(crate::pane::Typed {
                line: typed,
                deadline: crate::after(Instant::now(), TYPE_WAIT),
                last_output: None,
            });
            if pane.child.is_none() {
                pane.type_now();
            }
        }
        self.next_pane = next_pane;
        self.panes.insert(id, pane);
        Ok(id)
    }

    fn new_tab_id(&mut self) -> Result<TabId, String> {
        advance(&mut self.next_tab, "tab").map(TabId)
    }

    fn create_workspace(
        &mut self,
        name: Option<String>,
        cmd: &[String],
        ctx: Option<&Ctx>,
    ) -> Result<WsId, String> {
        let default_ctx = Ctx::default();
        let cwd = self.cwd_for(ctx.unwrap_or(&default_ctx), None);
        // The IDs are taken before the pane starts, so that none can run out
        // after, and are committed once it has.
        let (mut next_ws, mut next_tab) = (self.next_ws, self.next_tab);
        let id = WsId(advance(&mut next_ws, "workspace")?);
        let tab = TabId(advance(&mut next_tab, "tab")?);
        let pane = self.new_pane(cmd, &cwd, DEFAULT_SIZE)?;
        (self.next_ws, self.next_tab) = (next_ws, next_tab);
        let name = name.unwrap_or_else(|| format!("workspace-{}", id.0));
        self.workspaces.push(Workspace {
            id,
            name,
            tabs: vec![Tab {
                id: tab,
                name: "main".into(),
                root: Some(Node::Pane(pane)),
            }],
        });
        Ok(id)
    }

    // ------------------------------------------------------------- clients

    /// A client attaches, viewing `workspace` or the first one.
    pub fn attach(
        &mut self,
        rows: u16,
        cols: u16,
        workspace: Option<&str>,
    ) -> Result<ClientId, String> {
        let ws = match workspace {
            Some(name) => {
                self.resolve_ws(&command::parse_workspace(name).map_err(|Usage(e)| e)?)?
            }
            None => self
                .workspaces
                .first()
                .map(|w| w.id)
                .ok_or("the server has no workspace")?,
        };
        let id = ClientId(advance(&mut self.next_client, "client")?);
        let mut view = View::new(id, rows.clamp(1, 4096), cols.clamp(1, 4096), ws);
        if let Some(error) = &self.config_error {
            // The bar is narrow: the file's name, not its whole path, which
            // the server's log has.
            let shown = match &self.config_path {
                Some(path) => {
                    let name = path
                        .file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                    error.replacen(&path.display().to_string(), &name, 1)
                }
                None => error.clone(),
            };
            view.error(format!("config: {shown}"));
        }
        self.views.insert(id, view);
        self.settle();
        Ok(id)
    }

    pub fn detach(&mut self, id: ClientId) {
        self.views.remove(&id);
        self.settle();
    }

    pub fn resize(&mut self, id: ClientId, rows: u16, cols: u16) {
        if let Some(view) = self.views.get_mut(&id) {
            view.rows = rows.clamp(1, 4096);
            view.cols = cols.clamp(1, 4096);
            view.dirty = true;
        }
        self.settle();
    }

    // -------------------------------------------------------------- layout

    /// The area panes share on a client's screen: all but the bar.
    pub fn pane_area(view: &View) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: view.cols,
            h: view.rows.saturating_sub(1),
        }
    }

    /// Where each pane of a view's current tab is on its screen.
    pub fn placement(&self, view: &View) -> Placement {
        let area = Self::pane_area(view);
        let Some(root) = view
            .tab()
            .and_then(|t| self.tab(t))
            .and_then(|t| t.root.as_ref())
        else {
            return Placement::default();
        };
        if view.zoom
            && let Some(focus) = view.focus()
            && root.contains(focus)
            && area.w > 0
            && area.h > 0
        {
            return Placement {
                panes: vec![(focus, area)],
                separators: Vec::new(),
            };
        }
        layout::place(root, area)
    }

    /// The area to lay out a tab in for a command without a client: a
    /// client showing it, else any client, else a typical terminal.
    fn reference_area(&self, tab: TabId, ctx: &Ctx) -> Rect {
        let view = self
            .view_of(ctx)
            .filter(|v| v.tab() == Some(tab))
            .or_else(|| self.views.values().find(|v| v.tab() == Some(tab)))
            .or_else(|| self.view_of(ctx))
            .or_else(|| self.views.values().next());
        view.map_or(
            Rect {
                x: 0,
                y: 0,
                w: DEFAULT_SIZE.1,
                h: DEFAULT_SIZE.0,
            },
            Self::pane_area,
        )
    }

    // -------------------------------------------------------------- repair

    /// Brings every view back to something that exists, then sizes the PTYs:
    /// run after every event.
    pub fn settle(&mut self) {
        let first_ws = self.workspaces.first().map(|w| w.id);
        let ids: Vec<ClientId> = self.views.keys().copied().collect();
        for id in ids {
            self.repair(id, first_ws);
        }
        self.size_panes();
    }

    fn repair(&mut self, id: ClientId, first_ws: Option<WsId>) {
        let Some(view) = self.views.get(&id) else {
            return;
        };
        let mut workspace = view.workspace;
        if self.workspace(workspace).is_none() {
            let Some(first) = first_ws else { return };
            workspace = first;
        }
        let tabs: Vec<TabId> = self
            .workspace(workspace)
            .map(|w| w.tabs.iter().map(|t| t.id).collect())
            .unwrap_or_default();
        let tab = view
            .tab_of
            .get(&workspace)
            .copied()
            .filter(|t| tabs.contains(t))
            .or_else(|| tabs.first().copied());
        let panes = tab.map(|t| self.tab_panes(t)).unwrap_or_default();
        let focus = tab.and_then(|t| {
            view.focus_of
                .get(&t)
                .copied()
                .filter(|p| panes.contains(p))
                .or_else(|| view.last_of.get(&t).copied().filter(|p| panes.contains(p)))
                .or_else(|| panes.first().copied())
        });
        // What the view's mode refers to must still exist.
        let gone: Option<String> = match &view.mode {
            Mode::Copy(copy) => {
                if !self.panes.contains_key(&copy.pane) {
                    Some("copy mode ended: its pane closed".into())
                } else if Some(copy.pane) != focus {
                    Some("copy mode ended: its pane is no longer focused".into())
                } else {
                    self.panes
                        .get(&copy.pane)
                        .and_then(|p| copy.check(p.screen()).err())
                }
            }
            Mode::List(list) => list
                .about
                .as_ref()
                .filter(|a| !self.exists(a))
                .map(|a| format!("closed: {} is gone", describe(a))),
            Mode::Confirm(confirm) => (!self.exists(&confirm.about))
                .then(|| format!("closed: {} is gone", describe(&confirm.about))),
            Mode::Prompt(prompt) => match &prompt.purpose {
                crate::view::PromptFor::Rename(target) if !self.exists(target) => {
                    Some(format!("closed: {} is gone", describe(target)))
                }
                crate::view::PromptFor::Command | crate::view::PromptFor::Rename(_) => None,
            },
            // Bindings change under a client from the command line.
            Mode::Column { path, .. } if !path.is_empty() && !self.is_layer(path) => Some(format!(
                "closed: the layer {} is gone",
                self.keys_named(path)
            )),
            Mode::Repeat { path } if !self.repeats(path) => Some(format!(
                "closed: the repeat mode {} is gone",
                self.keys_named(path)
            )),
            Mode::Normal | Mode::Column { .. } | Mode::Repeat { .. } => None,
        };
        // A column shorter than it was keeps its selection within it.
        let last = match &view.mode {
            Mode::Column { path, .. } => {
                Some(crate::overlay::column_len(self, path).saturating_sub(1))
            }
            Mode::Normal
            | Mode::Repeat { .. }
            | Mode::List(_)
            | Mode::Prompt(_)
            | Mode::Confirm(_)
            | Mode::Copy(_) => None,
        };
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        if view.workspace != workspace {
            view.workspace = workspace;
            view.dirty = true;
        }
        if let Some(tab) = tab {
            if view.tab_of.get(&workspace) != Some(&tab) {
                view.tab_of.insert(workspace, tab);
                view.dirty = true;
            }
            match focus {
                Some(focus) if view.focus_of.get(&tab) != Some(&focus) => {
                    view.focus_of.insert(tab, focus);
                    view.dirty = true;
                }
                None => {
                    view.focus_of.remove(&tab);
                }
                _ => {}
            }
        }
        if let (Mode::Column { selected, .. }, Some(last)) = (&mut view.mode, last)
            && *selected > last
        {
            *selected = last;
            view.dirty = true;
        }
        if let Some(reason) = gone {
            view.mode = Mode::Normal;
            view.error(reason);
        }
    }

    /// Whether `path` is a layer: some binding's keys go on past it.
    fn is_layer(&self, path: &[KeyPress]) -> bool {
        self.config
            .bindings
            .iter()
            .any(|b| b.keys.len() > path.len() && b.keys.starts_with(path))
    }

    /// Whether the layer at `path` holds a repeating binding.
    fn repeats(&self, path: &[KeyPress]) -> bool {
        self.config.bindings.iter().any(|b| {
            b.repeat && b.keys.len() == path.len().saturating_add(1) && b.keys.starts_with(path)
        })
    }

    /// Keys after the prefix as they are typed: `C-b t`.
    fn keys_named(&self, path: &[KeyPress]) -> String {
        format!("{} {}", self.config.prefix, crate::config::keys_text(path))
    }

    /// A PTY is the smallest rectangle any client shows it in; a pane nobody
    /// shows keeps its size.
    fn size_panes(&mut self) {
        let mut sizes: BTreeMap<PaneId, (u16, u16)> = BTreeMap::new();
        for view in self.views.values() {
            for (pane, rect) in self.placement(view).panes {
                let entry = sizes.entry(pane).or_insert((rect.h, rect.w));
                entry.0 = entry.0.min(rect.h);
                entry.1 = entry.1.min(rect.w);
            }
        }
        for (id, (rows, cols)) in sizes {
            if let Some(pane) = self.panes.get_mut(&id)
                && pane.size != (rows.max(1), cols.max(1))
            {
                pane.resize(rows, cols);
                for view in self.views.values_mut() {
                    view.dirty = true;
                }
            }
        }
    }

    pub fn touch(&mut self) {
        for view in self.views.values_mut() {
            view.dirty = true;
        }
    }

    // ------------------------------------------------------------- closing

    /// Hangs up a pane's process; it is killed and reaped after `GRACE`.
    fn end(&mut self, pane: Pane) {
        if let Some(child) = pane.child {
            crate::process::hangup(child.pid);
            self.dying.push(Dying {
                pid: child.pid,
                master: Some(child.master),
                deadline: crate::after(Instant::now(), GRACE),
            });
        }
    }

    /// Removes a pane and its process. A tab its closing empties closes too,
    /// as does a workspace left with no tabs; a tab emptied by moving its
    /// panes out stays.
    pub fn close_pane(&mut self, id: PaneId, why: Option<String>) {
        let place = self.locate(id);
        if let Some((_, tab)) = place
            && let Some(t) = self.tab_mut(tab)
        {
            layout::remove(&mut t.root, id);
            if t.root.is_none() {
                self.remove_tab(tab);
            }
        }
        if let Some(pane) = self.panes.remove(&id) {
            if let Some(why) = why {
                for view in self.views.values_mut() {
                    if place.is_some_and(|(_, t)| view.tab() == Some(t)) {
                        view.info(format!("{id} {} {why}", pane.label()));
                    }
                }
            }
            self.end(pane);
        }
        self.after_close();
    }

    /// Removes a tab and whatever panes are in it.
    fn remove_tab(&mut self, tab: TabId) {
        let Some((w, t)) = self.find_tab(tab) else {
            return;
        };
        let panes = self.tab_panes(tab);
        let Some(ws) = self.workspaces.get_mut(w) else {
            return;
        };
        let ws_id = ws.id;
        // Tab IDs are unique: this removes the tab at `t`.
        ws.tabs.retain(|other| other.id != tab);
        // Views on the closed tab select its neighbour.
        let neighbour = ws
            .tabs
            .get(t)
            .or_else(|| t.checked_sub(1).and_then(|i| ws.tabs.get(i)))
            .map(|t| t.id);
        let empty = ws.tabs.is_empty();
        for view in self.views.values_mut() {
            if view.tab_of.get(&ws_id) == Some(&tab) {
                match neighbour {
                    Some(n) => {
                        view.tab_of.insert(ws_id, n);
                    }
                    None => {
                        view.tab_of.remove(&ws_id);
                    }
                }
            }
        }
        if empty {
            self.workspaces.retain(|w| w.id != ws_id);
        }
        for pane in panes {
            if let Some(pane) = self.panes.remove(&pane) {
                self.end(pane);
            }
        }
    }

    fn remove_workspace(&mut self, ws: WsId) {
        let tabs: Vec<TabId> = self
            .workspace(ws)
            .map(|w| w.tabs.iter().map(|t| t.id).collect())
            .unwrap_or_default();
        for tab in tabs {
            self.remove_tab(tab);
        }
        self.workspaces.retain(|w| w.id != ws);
    }

    fn after_close(&mut self) {
        if self.panes.is_empty()
            && !self
                .outbox
                .iter()
                .any(|o| matches!(o, Outgoing::Shutdown(_)))
        {
            self.outbox
                .push(Outgoing::Shutdown("the last pane closed".into()));
        }
        self.touch();
        self.settle();
    }

    /// A pane's program exited with `status`.
    pub fn exited(&mut self, id: PaneId, status: i32) {
        self.close_pane(id, Some(format!("exited with status {status}")));
    }

    /// Output from a pane's program.
    pub fn output(&mut self, id: PaneId, bytes: &[u8]) {
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        let dropped = pane.output(bytes);
        let place = self.locate(id);
        for view in self.views.values_mut() {
            if place.is_some_and(|(_, t)| view.tab() == Some(t)) {
                view.dirty = true;
                if dropped {
                    view.error(format!(
                        "{id} is not reading its input; a terminal reply was dropped"
                    ));
                }
            }
        }
    }

    /// Types held command lines whose wait is over; the next moment one is
    /// due.
    pub fn type_due(&mut self, now: Instant) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        for pane in self.panes.values_mut() {
            let Some(at) = pane.typed.as_ref().map(crate::pane::Typed::due_at) else {
                continue;
            };
            if at <= now {
                pane.type_now();
            } else {
                next = Some(next.map_or(at, |n| n.min(at)));
            }
        }
        next
    }

    /// The next moment a held command line is due, for the poll timeout.
    pub fn next_typing(&self) -> Option<Instant> {
        self.panes
            .values()
            .filter_map(|p| p.typed.as_ref().map(crate::pane::Typed::due_at))
            .min()
    }

    /// Everything the server shuts down: every pane is hung up.
    pub fn shutdown(&mut self) {
        let panes: Vec<PaneId> = self.panes.keys().copied().collect();
        for id in panes {
            if let Some(pane) = self.panes.remove(&id) {
                self.end(pane);
            }
        }
    }

    // ------------------------------------------------------------ commands

    /// Runs a command line from the CLI, a binding, a menu or the prompt.
    pub fn run(&mut self, argv: &[String], ctx: &Ctx) -> Outcome {
        let outcome = match command::parse(argv) {
            Err(Usage(message)) => Outcome {
                status: 2,
                stdout: String::new(),
                stderr: message,
            },
            Ok(command) => match self.execute(command, ctx) {
                Ok(stdout) => Outcome {
                    status: 0,
                    stdout,
                    stderr: String::new(),
                },
                Err(message) => Outcome {
                    status: 1,
                    stdout: String::new(),
                    stderr: message,
                },
            },
        };
        self.touch();
        self.settle();
        outcome
    }

    /// Why a command line cannot run now, if it cannot: menus and the
    /// command column dim such entries, and running one says why.
    pub fn unavailable(&self, argv: &[String], ctx: &Ctx) -> Option<String> {
        let command = match command::parse(argv) {
            Ok(command) => command,
            Err(Usage(message)) => return Some(message),
        };
        let view = self.view_of(ctx);
        let pane_count = view
            .and_then(View::tab)
            .map_or(0, |t| self.tab_panes(t).len());
        let tab_count = view
            .and_then(|v| self.workspace(v.workspace))
            .map_or(0, |w| w.tabs.len());
        let result: Result<(), String> = (|| {
            match &command {
                Command::SelectPane {
                    pick: Pick::Next | Pick::Previous | Pick::Last,
                    ..
                }
                | Command::ChoosePane { .. } => {
                    if pane_count < 2 {
                        return Err("only one pane".into());
                    }
                }
                Command::SelectTab {
                    pick: Pick::Next | Pick::Previous,
                    ..
                } => {
                    if tab_count < 2 {
                        return Err("only one tab".into());
                    }
                }
                Command::SelectWorkspace {
                    pick: Pick::Next | Pick::Previous,
                    ..
                } => {
                    if self.workspaces.len() < 2 {
                        return Err("only one workspace".into());
                    }
                }
                Command::Terminate { target } => {
                    let pane = self.pane_target(*target, ctx)?;
                    if self.panes.get(&pane).is_none_or(Pane::idle) {
                        return Err(format!("nothing is running in {pane} but its shell"));
                    }
                }
                Command::PasteBuffer { index, .. } => {
                    if self.buffers.get(*index).is_none() {
                        return Err("no copied text yet".into());
                    }
                }
                Command::KillPane { target }
                | Command::SwapPane { target, .. }
                | Command::MovePane { target, .. } => {
                    self.pane_target(*target, ctx)?;
                }
                Command::Ls { .. }
                | Command::KillServer
                | Command::ListKeys
                | Command::NewWorkspace { .. }
                | Command::NewTab { .. }
                | Command::Split { .. }
                | Command::KillTab { .. }
                | Command::KillWorkspace { .. }
                | Command::Rename { .. }
                | Command::ResizePane { .. }
                | Command::SendKeys { .. }
                | Command::CapturePane { .. }
                | Command::CaptureClient { .. }
                | Command::Reorder { .. }
                | Command::Set { .. }
                | Command::Bind { .. }
                | Command::Unbind { .. }
                | Command::UnbindAll
                | Command::Reload
                | Command::ListBuffers
                | Command::ShowBuffer { .. }
                | Command::Detach { .. }
                | Command::CommandColumn { .. }
                | Command::ChooseTab { .. }
                | Command::ChooseWorkspace { .. }
                | Command::Menu { .. }
                | Command::CommandPrompt { .. }
                | Command::CopyMode { .. }
                | Command::RenamePrompt { .. }
                | Command::ConfirmClose { .. }
                | Command::Zoom { .. }
                | Command::SelectPane { .. }
                | Command::SelectTab { .. }
                | Command::SelectWorkspace { .. } => {}
            }
            Ok(())
        })();
        result.err()
    }

    fn execute(&mut self, command: Command, ctx: &Ctx) -> Result<String, String> {
        match command {
            Command::Ls { json } => Ok(if json { self.ls_json() } else { self.ls_text() }),
            Command::KillServer => {
                self.outbox
                    .push(Outgoing::Shutdown("stopped by fux kill-server".into()));
                Ok(String::new())
            }
            Command::ListKeys => {
                let mut out = String::from(
                    "Keys, for send-keys and the prefix (with C-, M-, S- prefixes, or any character):\n",
                );
                out.push_str(&crate::keys::all_names().join(" "));
                out.push_str("\n\nBindings (after the prefix, ");
                out.push_str(&self.config.prefix.to_string());
                out.push_str("; each key a letter, in either case):\n");
                for binding in &self.config.bindings {
                    out.push_str(&format!(
                        "{:>8}  {}{}\n",
                        crate::config::keys_text(&binding.keys),
                        crate::words::join(&binding.command),
                        if binding.repeat { " (repeats)" } else { "" }
                    ));
                }
                Ok(out)
            }
            Command::NewWorkspace { name, cmd } => {
                if let Some(name) = &name {
                    self.check_name(name)?;
                }
                let ws = self.create_workspace(name, &cmd, Some(ctx))?;
                if let Some(view) = ctx.client.and_then(|c| self.views.get_mut(&c)) {
                    view.workspace = ws;
                    view.zoom = false;
                }
                Ok(format!("{ws}\n"))
            }
            Command::NewTab { target, name, cmd } => {
                let ws = self.ws_target(target.as_ref(), ctx)?;
                if let Some(name) = &name {
                    self.check_name(name)?;
                }
                let cwd = self.cwd_for(ctx, None);
                // As for a workspace: the ID first, committed after the pane.
                let mut next_tab = self.next_tab;
                let id = TabId(advance(&mut next_tab, "tab")?);
                let pane = self.new_pane(&cmd, &cwd, DEFAULT_SIZE)?;
                self.next_tab = next_tab;
                let index = self.ws_index(ws).ok_or("the workspace is gone")?;
                let Some(workspace) = self.workspaces.get_mut(index) else {
                    return Err("the workspace is gone".into());
                };
                let number = workspace.tabs.len().saturating_add(1);
                let name = name.unwrap_or_else(|| format!("tab-{number}"));
                workspace.tabs.push(Tab {
                    id,
                    name,
                    root: Some(Node::Pane(pane)),
                });
                if let Some(view) = ctx.client.and_then(|c| self.views.get_mut(&c)) {
                    view.workspace = ws;
                    view.tab_of.insert(ws, id);
                    view.zoom = false;
                }
                Ok(format!("{id}\n"))
            }
            Command::Split { axis, target, cmd } => {
                let target = self.pane_target(target, ctx)?;
                let (_, tab) = self.locate(target).ok_or("the pane is in no tab")?;
                let size = self.panes.get(&target).map_or(DEFAULT_SIZE, |p| p.size);
                let cwd = self.cwd_for(ctx, Some(target));
                let pane = self.new_pane(&cmd, &cwd, size)?;
                if let Some(t) = self.tab_mut(tab) {
                    layout::split(&mut t.root, target, pane, axis, true);
                }
                // The splitting client follows the new pane; from the CLI,
                // clients focused on the split pane do.
                for view in self.views.values_mut() {
                    let follows = match ctx.client {
                        Some(client) => view.id == client,
                        None => view.focus_of.get(&tab) == Some(&target),
                    };
                    if follows {
                        view.set_focus(tab, pane);
                        view.zoom = false;
                    }
                }
                Ok(format!("{pane}\n"))
            }
            Command::KillPane { target } => {
                let pane = self.pane_target(target, ctx)?;
                self.close_pane(pane, None);
                Ok(String::new())
            }
            Command::KillTab { target } => {
                let tab = self.tab_target(target, ctx)?;
                self.remove_tab(tab);
                self.after_close();
                Ok(String::new())
            }
            Command::KillWorkspace { target } => {
                let ws = self.ws_target(target.as_ref(), ctx)?;
                self.remove_workspace(ws);
                self.after_close();
                Ok(String::new())
            }
            Command::Rename { target, name } => self.rename(&target, name).map(|()| String::new()),
            Command::MovePane { target, to } => self.move_pane(target, to, ctx),
            Command::SwapPane { target, with } => {
                let source = self.pane_target(target, ctx)?;
                let other = match with {
                    SwapWith::Pane(p) => {
                        if !self.panes.contains_key(&p) {
                            return Err(format!("no pane {p}"));
                        }
                        p
                    }
                    SwapWith::Toward(direction) => self.neighbor(source, direction, ctx)?,
                };
                self.swap(source, other)?;
                Ok(String::new())
            }
            Command::ResizePane {
                target,
                direction,
                amount,
            } => {
                let pane = self.pane_target(target, ctx)?;
                let (_, tab) = self.locate(pane).ok_or("the pane is in no tab")?;
                let area = self.reference_area(tab, ctx);
                let Some(root) = self.tab_mut(tab).and_then(|t| t.root.as_mut()) else {
                    return Err("the tab is empty".into());
                };
                if layout::resize(root, area, pane, direction, amount) {
                    Ok(String::new())
                } else {
                    Err(format!("{pane} has no border to move {}", direction.name()))
                }
            }
            Command::SendKeys {
                target,
                literal,
                keys,
            } => {
                let pane = self.pane_target(target, ctx)?;
                let Some(p) = self.panes.get_mut(&pane) else {
                    return Err(format!("no pane {pane}"));
                };
                // Each argument is a key name (`C-c`, `Enter`, `a`), or, as in
                // tmux, text sent as it is; `-l` makes every argument text.
                let mut bytes = Vec::new();
                let application = p.screen().application_cursor();
                for key in &keys {
                    match key.parse::<KeyPress>() {
                        Ok(press) if !literal => {
                            bytes.extend(crate::encode::key_bytes(press, application))
                        }
                        _ => bytes.extend_from_slice(key.as_bytes()),
                    }
                }
                p.input.push(bytes)?;
                Ok(String::new())
            }
            Command::CapturePane {
                target,
                history,
                json,
            } => {
                let pane = self.pane_target(target, ctx)?;
                let p = self
                    .panes
                    .get(&pane)
                    .ok_or_else(|| format!("no pane {pane}"))?;
                Ok(capture(p, history, json))
            }
            Command::CaptureClient { client, json } => {
                let client = self.client_target(client, ctx)?;
                let grid = crate::render::compose(self, client)
                    .ok_or_else(|| format!("no client {client}"))?;
                Ok(capture_client(client, &grid, json))
            }
            Command::Terminate { target } => {
                let pane = self.pane_target(target, ctx)?;
                let p = self
                    .panes
                    .get(&pane)
                    .ok_or_else(|| format!("no pane {pane}"))?;
                let child = p.child.as_ref().ok_or("the pane has no process")?;
                match crate::process::foreground(&child.master) {
                    Some(group) if group != child.pid => {
                        crate::process::terminate(group)?;
                        Ok(String::new())
                    }
                    _ => Err(format!("nothing is running in {pane} but its shell")),
                }
            }
            Command::Reorder {
                kind,
                target,
                forward,
            } => {
                let target = self.any_target(kind, target.as_ref(), ctx)?;
                self.reorder(&target, forward).map(|()| String::new())
            }
            Command::Set { argv } | Command::Bind { argv } | Command::Unbind { argv } => {
                self.config.apply(&argv).map(|()| String::new())
            }
            Command::UnbindAll => {
                self.config.bindings.clear();
                Ok(String::new())
            }
            Command::Reload => {
                let path = self.config_path.clone().ok_or("no config file to reload")?;
                match Config::from_file(&path) {
                    Ok(config) => {
                        self.config = config;
                        self.config_error = None;
                        Ok(format!("reloaded {}\n", path.display()))
                    }
                    Err(error) => Err(format!("{error}; the previous configuration is kept")),
                }
            }
            Command::ListBuffers => Ok(self
                .buffers
                .iter()
                .enumerate()
                .map(|(i, text)| {
                    let preview: String = text
                        .chars()
                        .map(|c| if c.is_control() { ' ' } else { c })
                        .take(50)
                        .collect();
                    format!("{i}: {} bytes: {preview}\n", text.len())
                })
                .collect()),
            Command::ShowBuffer { index } => self
                .buffers
                .get(index)
                .cloned()
                .ok_or_else(|| format!("no buffer {index}")),
            Command::PasteBuffer { index, target } => {
                let text = self
                    .buffers
                    .get(index)
                    .cloned()
                    .ok_or_else(|| format!("no buffer {index}"))?;
                let pane = self.pane_target(target, ctx)?;
                let p = self
                    .panes
                    .get_mut(&pane)
                    .ok_or_else(|| format!("no pane {pane}"))?;
                let bracketed = p.screen().bracketed_paste();
                p.input.push(crate::encode::paste(&text, bracketed))?;
                Ok(String::new())
            }
            Command::Detach { client } => {
                let client = self.client_target(client, ctx)?;
                self.views.remove(&client);
                self.outbox.push(Outgoing::Exit(client, "detached".into()));
                Ok(String::new())
            }
            Command::Zoom { client } => {
                let client = self.client_target(client, ctx)?;
                let view = self.view_mut(client)?;
                view.zoom = !view.zoom;
                Ok(String::new())
            }
            Command::SelectPane { client, pick } => {
                let client = self.client_target(client, ctx)?;
                self.select_pane(client, pick)
            }
            Command::SelectTab { client, pick } => {
                let client = self.client_target(client, ctx)?;
                self.select_tab(client, pick)
            }
            Command::SelectWorkspace { client, pick } => {
                let client = self.client_target(client, ctx)?;
                self.select_workspace(client, pick)
            }
            Command::CommandColumn { client } => {
                let client = self.client_target(client, ctx)?;
                self.view_mut(client)?.mode = Mode::Column {
                    path: Vec::new(),
                    selected: 0,
                };
                Ok(String::new())
            }
            Command::CommandPrompt { client } => {
                let client = self.client_target(client, ctx)?;
                crate::overlay::open_prompt(
                    self,
                    client,
                    crate::view::PromptFor::Command,
                    ":".into(),
                    String::new(),
                )
            }
            Command::RenamePrompt {
                client,
                kind,
                target,
            } => {
                let client = self.client_target(client, ctx)?;
                let ctx = Ctx::client(client);
                let target = self.any_target(kind, target.as_ref(), &ctx)?;
                let current = self.name_of(&target);
                crate::overlay::open_prompt(
                    self,
                    client,
                    crate::view::PromptFor::Rename(target.clone()),
                    format!("rename {} {}", kind.name(), describe(&target)),
                    current,
                )
            }
            Command::ConfirmClose {
                client,
                kind,
                target,
            } => {
                let client = self.client_target(client, ctx)?;
                let ctx = Ctx::client(client);
                let target = self.any_target(kind, target.as_ref(), &ctx)?;
                crate::overlay::open_confirm(self, client, target)
            }
            Command::Menu {
                client,
                kind,
                target,
            } => {
                let client = self.client_target(client, ctx)?;
                let ctx = Ctx::client(client);
                let target = self.any_target(kind, target.as_ref(), &ctx)?;
                crate::overlay::open_menu(self, client, target)
            }
            Command::ChooseTab {
                client,
                moving,
                moving_now,
            } => {
                let client = self.client_target(client, ctx)?;
                let moving = self.moving(client, moving, moving_now)?;
                crate::overlay::open_tab_chooser(self, client, moving)
            }
            Command::ChooseWorkspace {
                client,
                moving,
                moving_now,
            } => {
                let client = self.client_target(client, ctx)?;
                let moving = self.moving(client, moving, moving_now)?;
                crate::overlay::open_workspace_chooser(self, client, moving)
            }
            Command::ChoosePane { client, target } => {
                let client = self.client_target(client, ctx)?;
                let ctx = Ctx::client(client);
                let source = self.pane_target(target, &ctx)?;
                crate::overlay::open_pane_chooser(self, client, source)
            }
            Command::CopyMode { client } => {
                let client = self.client_target(client, ctx)?;
                crate::copy::enter(self, client)
            }
        }
    }

    fn moving(
        &self,
        client: ClientId,
        moving: Option<PaneId>,
        now: bool,
    ) -> Result<Option<PaneId>, String> {
        match (moving, now) {
            (Some(p), _) => self.pane_target(Some(p), &Ctx::default()).map(Some),
            (None, true) => self.pane_target(None, &Ctx::client(client)).map(Some),
            (None, false) => Ok(None),
        }
    }

    fn check_name(&self, name: &str) -> Result<(), String> {
        if name.is_empty() {
            return Err("a name cannot be empty".into());
        }
        if name.chars().any(char::is_control) {
            return Err("a name cannot contain control characters".into());
        }
        if name.len() > 256 {
            return Err("a name is at most 256 bytes".into());
        }
        Ok(())
    }

    pub fn name_of(&self, target: &AnyRef) -> String {
        match target {
            AnyRef::Pane(p) => self.panes.get(p).map(|p| p.name.clone()),
            AnyRef::Tab(t) => self.tab(*t).map(|t| t.name.clone()),
            AnyRef::Workspace(w) => self
                .resolve_ws(w)
                .ok()
                .and_then(|w| self.workspace(w))
                .map(|w| w.name.clone()),
        }
        .unwrap_or_default()
    }

    fn rename(&mut self, target: &AnyRef, name: String) -> Result<(), String> {
        self.check_name(&name)?;
        match target {
            AnyRef::Pane(p) => {
                self.panes
                    .get_mut(p)
                    .ok_or_else(|| format!("no pane {p}"))?
                    .name = name;
            }
            AnyRef::Tab(t) => {
                self.tab_mut(*t).ok_or_else(|| format!("no tab {t}"))?.name = name;
            }
            AnyRef::Workspace(w) => {
                let id = self.resolve_ws(w)?;
                if let WsRef::Name(_) = w {
                    // Renaming by name is fine; a clash is refused.
                }
                if self
                    .workspaces
                    .iter()
                    .any(|ws| ws.name == name && ws.id != id)
                {
                    return Err(format!("another workspace is named {name:?}"));
                }
                let index = self.ws_index(id).ok_or("no such workspace")?;
                if let Some(ws) = self.workspaces.get_mut(index) {
                    ws.name = name;
                }
            }
        }
        Ok(())
    }

    fn neighbor(&self, from: PaneId, direction: Direction, ctx: &Ctx) -> Result<PaneId, String> {
        let (_, tab) = self.locate(from).ok_or("the pane is in no tab")?;
        let area = self.reference_area(tab, ctx);
        let root = self
            .tab(tab)
            .and_then(|t| t.root.as_ref())
            .ok_or("the tab is empty")?;
        let placement = layout::place(root, area);
        layout::neighbor(&placement, from, direction)
            .ok_or_else(|| format!("no pane {} of {from}", direction.name()))
    }

    fn swap(&mut self, a: PaneId, b: PaneId) -> Result<(), String> {
        if a == b {
            return Ok(());
        }
        let (_, ta) = self.locate(a).ok_or("the pane is in no tab")?;
        let (_, tb) = self.locate(b).ok_or("the other pane is in no tab")?;
        if ta == tb {
            if let Some(root) = self.tab_mut(ta).and_then(|t| t.root.as_mut()) {
                layout::swap(root, a, b);
            }
        } else {
            let hole = PaneId(u32::MAX);
            if let Some(root) = self.tab_mut(ta).and_then(|t| t.root.as_mut()) {
                root.replace(a, hole);
            }
            if let Some(root) = self.tab_mut(tb).and_then(|t| t.root.as_mut()) {
                root.replace(b, a);
            }
            if let Some(root) = self.tab_mut(ta).and_then(|t| t.root.as_mut()) {
                root.replace(hole, b);
            }
        }
        Ok(())
    }

    fn move_pane(
        &mut self,
        target: Option<PaneId>,
        to: MoveTo,
        ctx: &Ctx,
    ) -> Result<String, String> {
        let pane = self.pane_target(target, ctx)?;
        let (source_ws, source_tab) = self.locate(pane).ok_or("the pane is in no tab")?;
        if let MoveTo::Beside(direction) = to {
            let destination = self.neighbor(pane, direction, ctx)?;
            let tab = self.tab_mut(source_tab).ok_or("the tab is gone")?;
            layout::remove(&mut tab.root, pane);
            let after = matches!(direction, Direction::Right | Direction::Down);
            layout::split(&mut tab.root, destination, pane, Axis::of(direction), after);
            return Ok(String::new());
        }
        let (ws, tab) = match to {
            MoveTo::Tab(tab) => {
                let ws = self
                    .tab_workspace(tab)
                    .ok_or_else(|| format!("no tab {tab}"))?;
                (ws, tab)
            }
            MoveTo::Workspace(r) => {
                let ws = self.resolve_ws(&r)?;
                let first = self
                    .workspace(ws)
                    .and_then(|w| w.tabs.first())
                    .map(|t| t.id);
                let tab = match first {
                    Some(tab) => tab,
                    None => {
                        let id = self.new_tab_id()?;
                        let index = self.ws_index(ws).ok_or("the workspace is gone")?;
                        if let Some(w) = self.workspaces.get_mut(index) {
                            w.tabs.push(Tab {
                                id,
                                name: "main".into(),
                                root: None,
                            });
                        }
                        id
                    }
                };
                (ws, tab)
            }
            MoveTo::NewTab => {
                let id = self.new_tab_id()?;
                let index = self.ws_index(source_ws).ok_or("the workspace is gone")?;
                if let Some(w) = self.workspaces.get_mut(index) {
                    let name = format!("tab-{}", w.tabs.len().saturating_add(1));
                    w.tabs.push(Tab {
                        id,
                        name,
                        root: None,
                    });
                }
                (source_ws, id)
            }
            MoveTo::NewWorkspace => {
                let id = WsId(advance(&mut self.next_ws, "workspace")?);
                let tab = self.new_tab_id()?;
                self.workspaces.push(Workspace {
                    id,
                    name: format!("workspace-{}", id.0),
                    tabs: vec![Tab {
                        id: tab,
                        name: "main".into(),
                        root: None,
                    }],
                });
                (id, tab)
            }
            MoveTo::Beside(_) => return Ok(String::new()),
        };
        if tab == source_tab {
            return Ok(String::new());
        }
        if let Some(t) = self.tab_mut(source_tab) {
            layout::remove(&mut t.root, pane);
        }
        let destination = self.tab_mut(tab).ok_or("the destination tab is gone")?;
        destination.root = Some(match destination.root.take() {
            None => Node::Pane(pane),
            Some(root) => {
                let mut node = Node::Split {
                    axis: Axis::Horizontal,
                    children: vec![(layout::WEIGHT, root), (layout::WEIGHT, Node::Pane(pane))],
                };
                layout::normalize(&mut node);
                node
            }
        });
        // The moving client follows its pane.
        if let Some(view) = ctx.client.and_then(|c| self.views.get_mut(&c)) {
            view.workspace = ws;
            view.tab_of.insert(ws, tab);
            view.set_focus(tab, pane);
            view.zoom = false;
        }
        Ok(format!("{tab}\n"))
    }

    fn reorder(&mut self, target: &AnyRef, forward: bool) -> Result<(), String> {
        let step = |index: usize, len: usize| -> Option<usize> {
            if forward {
                index.checked_add(1).filter(|next| *next < len)
            } else {
                index.checked_sub(1)
            }
        };
        match target {
            AnyRef::Pane(p) => {
                let (_, tab) = self.locate(*p).ok_or("the pane is in no tab")?;
                let panes = self.tab_panes(tab);
                let index = panes
                    .iter()
                    .position(|x| x == p)
                    .ok_or("the pane is gone")?;
                let other = step(index, panes.len())
                    .and_then(|i| panes.get(i))
                    .copied()
                    .ok_or("the pane is already at that end")?;
                self.swap(*p, other)
            }
            AnyRef::Tab(t) => {
                let (w, index) = self.find_tab(*t).ok_or_else(|| format!("no tab {t}"))?;
                let ws = self.workspaces.get_mut(w).ok_or("the workspace is gone")?;
                let other = step(index, ws.tabs.len()).ok_or("the tab is already at that end")?;
                let [a, b] = ws
                    .tabs
                    .get_disjoint_mut([index, other])
                    .map_err(|_| "the tab is gone")?;
                std::mem::swap(a, b);
                Ok(())
            }
            AnyRef::Workspace(r) => {
                let id = self.resolve_ws(r)?;
                let index = self.ws_index(id).ok_or("the workspace is gone")?;
                let other = step(index, self.workspaces.len())
                    .ok_or("the workspace is already at that end")?;
                let [a, b] = self
                    .workspaces
                    .get_disjoint_mut([index, other])
                    .map_err(|_| "the workspace is gone")?;
                std::mem::swap(a, b);
                Ok(())
            }
        }
    }

    fn select_pane(&mut self, client: ClientId, pick: Pick<PaneId>) -> Result<String, String> {
        let view = self.views.get(&client).ok_or("no such client")?;
        let tab = view.tab().ok_or("no tab")?;
        let panes = self.tab_panes(tab);
        let current = view.focus();
        let target = match pick {
            Pick::Id(p) => {
                let (ws, tab) = self.locate(p).ok_or_else(|| format!("no pane {p}"))?;
                let view = self.view_mut(client)?;
                view.workspace = ws;
                view.tab_of.insert(ws, tab);
                view.set_focus(tab, p);
                view.zoom = false;
                return Ok(String::new());
            }
            Pick::Next | Pick::Previous => {
                if panes.len() < 2 {
                    return Err("only one pane".into());
                }
                let index = current
                    .and_then(|c| panes.iter().position(|p| *p == c))
                    .unwrap_or(0);
                let next = round(index, panes.len(), matches!(pick, Pick::Next));
                panes.get(next).copied().ok_or("no pane")?
            }
            Pick::Last => view
                .last_of
                .get(&tab)
                .copied()
                .filter(|p| panes.contains(p) && Some(*p) != current)
                .ok_or("no previously focused pane")?,
            Pick::Toward(direction) => {
                let placement = layout::place(
                    self.tab(tab)
                        .and_then(|t| t.root.as_ref())
                        .ok_or("the tab is empty")?,
                    Self::pane_area(view),
                );
                let from = current.ok_or("no pane")?;
                layout::neighbor(&placement, from, direction)
                    .ok_or_else(|| format!("no pane {} of {from}", direction.name()))?
            }
        };
        let view = self.view_mut(client)?;
        view.set_focus(tab, target);
        view.zoom = false;
        Ok(String::new())
    }

    fn select_tab(&mut self, client: ClientId, pick: Pick<TabId>) -> Result<String, String> {
        let view = self.views.get(&client).ok_or("no such client")?;
        let ws = view.workspace;
        let tabs: Vec<TabId> = self
            .workspace(ws)
            .map(|w| w.tabs.iter().map(|t| t.id).collect())
            .unwrap_or_default();
        let target_ws;
        let target = match pick {
            Pick::Id(t) => {
                target_ws = self.tab_workspace(t).ok_or_else(|| format!("no tab {t}"))?;
                t
            }
            Pick::Next | Pick::Previous => {
                if tabs.len() < 2 {
                    return Err("only one tab".into());
                }
                target_ws = ws;
                let index = view
                    .tab()
                    .and_then(|c| tabs.iter().position(|t| *t == c))
                    .unwrap_or(0);
                let next = round(index, tabs.len(), matches!(pick, Pick::Next));
                tabs.get(next).copied().ok_or("no tab")?
            }
            Pick::Last | Pick::Toward(_) => {
                return Err("select-tab takes -t, --next or --previous".into());
            }
        };
        let view = self.view_mut(client)?;
        view.workspace = target_ws;
        view.tab_of.insert(target_ws, target);
        view.zoom = false;
        Ok(String::new())
    }

    fn select_workspace(&mut self, client: ClientId, pick: Pick<WsRef>) -> Result<String, String> {
        let current = self.views.get(&client).ok_or("no such client")?.workspace;
        let target = match pick {
            Pick::Id(r) => self.resolve_ws(&r)?,
            Pick::Next | Pick::Previous => {
                let len = self.workspaces.len();
                if len < 2 {
                    return Err("only one workspace".into());
                }
                let index = self.ws_index(current).unwrap_or(0);
                let next = round(index, len, matches!(pick, Pick::Next));
                self.workspaces
                    .get(next)
                    .map(|w| w.id)
                    .ok_or("no workspace")?
            }
            Pick::Last | Pick::Toward(_) => {
                return Err("select-workspace takes -t, --next or --previous".into());
            }
        };
        let view = self.view_mut(client)?;
        view.workspace = target;
        view.zoom = false;
        Ok(String::new())
    }

    // ------------------------------------------------------------------ ls

    fn ls_text(&self) -> String {
        let mut out = String::new();
        for ws in &self.workspaces {
            out.push_str(&format!("{} {}\n", ws.id, ws.name));
            for tab in &ws.tabs {
                out.push_str(&format!(
                    "  {} {}{}\n",
                    tab.id,
                    tab.name,
                    if tab.root.is_none() { " (empty)" } else { "" }
                ));
                for pane in self.tab_panes(tab.id) {
                    if let Some(p) = self.panes.get(&pane) {
                        out.push_str(&format!(
                            "    {} {} {}x{}{}\n",
                            pane,
                            p.label(),
                            p.size.1,
                            p.size.0,
                            p.child
                                .as_ref()
                                .map(|c| format!(" pid {}", c.pid))
                                .unwrap_or_default()
                        ));
                    }
                }
            }
        }
        for view in self.views.values() {
            out.push_str(&format!(
                "client {} {}x{} {} {} {}\n",
                view.id,
                view.cols,
                view.rows,
                view.workspace,
                view.tab()
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "-".into()),
                view.focus()
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "-".into()),
            ));
        }
        out
    }

    fn ls_json(&self) -> String {
        let panes = |tab: TabId| {
            Json::Array(
                self.tab_panes(tab)
                    .into_iter()
                    .filter_map(|id| self.panes.get(&id))
                    .map(|p| {
                        Json::Object(vec![
                            ("id", Json::str(p.id.to_string())),
                            ("name", Json::str(p.name.clone())),
                            ("title", Json::str(p.title.clone())),
                            ("rows", Json::Number(i64::from(p.size.0))),
                            ("cols", Json::Number(i64::from(p.size.1))),
                            (
                                "pid",
                                p.child.as_ref().map_or(Json::Null, |c| {
                                    Json::Number(i64::from(c.pid.as_raw()))
                                }),
                            ),
                        ])
                    })
                    .collect(),
            )
        };
        let workspaces = self
            .workspaces
            .iter()
            .map(|ws| {
                Json::Object(vec![
                    ("id", Json::str(ws.id.to_string())),
                    ("name", Json::str(ws.name.clone())),
                    (
                        "tabs",
                        Json::Array(
                            ws.tabs
                                .iter()
                                .map(|t| {
                                    Json::Object(vec![
                                        ("id", Json::str(t.id.to_string())),
                                        ("name", Json::str(t.name.clone())),
                                        ("panes", panes(t.id)),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                ])
            })
            .collect();
        let opt = |v: Option<String>| v.map_or(Json::Null, Json::str);
        let clients = self
            .views
            .values()
            .map(|v| {
                Json::Object(vec![
                    ("id", Json::str(v.id.to_string())),
                    ("rows", Json::Number(i64::from(v.rows))),
                    ("cols", Json::Number(i64::from(v.cols))),
                    ("workspace", Json::str(v.workspace.to_string())),
                    ("tab", opt(v.tab().map(|t| t.to_string()))),
                    ("pane", opt(v.focus().map(|p| p.to_string()))),
                    ("zoom", Json::Bool(v.zoom)),
                ])
            })
            .collect();
        let mut text = Json::Object(vec![
            ("workspaces", Json::Array(workspaces)),
            ("clients", Json::Array(clients)),
        ])
        .render();
        text.push('\n');
        text
    }
}

/// `%3`, `@2`, `+1` or a workspace's name.
pub fn describe(target: &AnyRef) -> String {
    match target {
        AnyRef::Pane(p) => p.to_string(),
        AnyRef::Tab(t) => t.to_string(),
        AnyRef::Workspace(WsRef::Id(w)) => w.to_string(),
        AnyRef::Workspace(WsRef::Name(n)) => n.clone(),
    }
}

/// Takes the next ID from `counter`. IDs are never reused, so one that
/// would wrap round is an error instead.
fn advance(counter: &mut u32, what: &str) -> Result<u32, String> {
    let id = *counter;
    *counter = counter
        .checked_add(1)
        .ok_or_else(|| format!("no {what} IDs are left"))?;
    Ok(id)
}

/// The index after `index` among `len`, or before it, going round.
fn round(index: usize, len: usize, forward: bool) -> usize {
    if forward {
        index.checked_add(1).filter(|next| *next < len).unwrap_or(0)
    } else {
        index
            .checked_sub(1)
            .unwrap_or_else(|| len.saturating_sub(1))
    }
}

/// The text of a pane's screen, and `history` lines before it.
fn capture(pane: &Pane, history: Option<usize>, json: bool) -> String {
    let screen = pane.screen();
    let (rows, cols) = screen.size();
    let back = history.unwrap_or(0).min(screen.history_len());
    let window = screen.window(back, rows, cols);
    let mut lines = Vec::new();
    // History rows above the screen, then the screen itself.
    for offset in (0..back).rev() {
        if let Some(row) = usize::from(rows)
            .checked_add(offset)
            .and_then(|i| screen.row_from_bottom(i))
        {
            lines.push(row_text(row.cells));
        }
    }
    let live = screen.window(0, rows, cols);
    let _ = window;
    for y in 0..rows {
        lines.push(live.row(y).map(|r| row_text(r.cells)).unwrap_or_default());
    }
    if json {
        let (cy, cx) = screen.cursor_position();
        let mut text = Json::Object(vec![
            ("pane", Json::str(pane.id.to_string())),
            ("rows", Json::Number(i64::from(rows))),
            ("cols", Json::Number(i64::from(cols))),
            (
                "cursor",
                Json::Array(vec![
                    Json::Number(i64::from(cy)),
                    Json::Number(i64::from(cx)),
                ]),
            ),
            (
                "lines",
                Json::Array(lines.into_iter().map(Json::String).collect()),
            ),
        ])
        .render();
        text.push('\n');
        return text;
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// What a client's terminal shows, row by row, as `capture-client` prints
/// it.
fn capture_client(client: ClientId, grid: &crate::render::Grid, json: bool) -> String {
    let lines: Vec<String> = (0..grid.rows).map(|y| grid.row_text(y)).collect();
    if json {
        let cursor = grid.cursor.map_or(Json::Null, |(y, x)| {
            Json::Array(vec![Json::Number(i64::from(y)), Json::Number(i64::from(x))])
        });
        let mut text = Json::Object(vec![
            ("client", Json::str(client.to_string())),
            ("rows", Json::Number(i64::from(grid.rows))),
            ("cols", Json::Number(i64::from(grid.cols))),
            ("cursor", cursor),
            (
                "lines",
                Json::Array(lines.into_iter().map(Json::String).collect()),
            ),
        ])
        .render();
        text.push('\n');
        return text;
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// A row's text, wide glyphs whole and trailing blanks trimmed.
pub fn row_text(cells: &[fux_vt::Cell]) -> String {
    let mut line = String::new();
    for cell in cells {
        if cell.is_wide_continuation() {
            continue;
        }
        line.push_str(if cell.has_contents() {
            cell.contents()
        } else {
            " "
        });
    }
    line.trim_end_matches(' ').to_owned()
}
