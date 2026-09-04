mod router;

pub use router::{Action, Command, InputRouter, MouseEvent};

use crate::state::{
    AgentState, AgentStatus, LayoutTree, MouseMode, PaneId, PaneView, Rect, Tab, TabId,
    WorkspaceState,
};
use koh::pty::Pty;
use koh::server::{ChangeSignal, ClientId, SessionHost};
use koh::terminal::{DEFAULT_COLS, DEFAULT_ROWS, ServerTerminal};
use std::collections::BTreeMap;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use tokio::sync::mpsc;

fn history_row_units() -> usize {
    usize::from(crate::state::MAX_DIM).saturating_mul(
        std::mem::size_of::<crate::state::Cell>().saturating_add(crate::state::MAX_CELL_TEXT_BYTES),
    )
}

fn worst_case_state_units() -> usize {
    crate::state::MAX_TOTAL_CELLS
        .saturating_mul(
            std::mem::size_of::<crate::state::Cell>()
                .saturating_add(crate::state::MAX_CELL_TEXT_BYTES),
        )
        .saturating_add(2 * 1024 * 1024)
}

#[derive(Clone)]
pub struct WorkspaceControl {
    inner: Arc<Mutex<WorkspaceHost>>,
}

pub struct WorkspaceSession {
    inner: Arc<Mutex<WorkspaceHost>>,
}

pub trait WorkspaceEventSink: Send + Sync + 'static {
    fn publish(&self, event: crate::control::Event);
}

pub const DEFAULT_SCROLLBACK: usize = 10_000;

struct PaneRuntime {
    pty: Option<Pty>,
    terminal: ServerTerminal,
    alive: bool,
    exit_code: Option<u32>,
    close_event_published: bool,
    last_report: Option<zor::osc::Report>,
    history_limit: usize,
    history_reserved_units: usize,
    last_bell_count: u64,
    last_clipboard: String,
    last_output_event_ms: Option<u64>,
    command: Vec<String>,
    cwd: PathBuf,
    geometry: Rect,
}

struct PendingDrain {
    pane: PaneId,
    receiver: mpsc::Receiver<Vec<u8>>,
}

#[derive(Default)]
struct RouterTimerState {
    deadline: Option<std::time::Instant>,
    shutdown: bool,
}

#[derive(Default)]
struct Shared {
    state: WorkspaceState,
    panes: BTreeMap<PaneId, Arc<Mutex<PaneRuntime>>>,
    event_sink: Option<Arc<dyn WorkspaceEventSink>>,
    resources: crate::config::ResourceLimits,
    final_snapshot_pending: bool,
    changed: Option<ChangeSignal>,
    /// Bytes promised to an in-flight geometry transaction while PTYs are resized.
    resource_reservation: usize,
}

type ExternalProcesses = Arc<Mutex<BTreeMap<i32, Arc<Mutex<Option<std::process::Child>>>>>>;

pub struct WorkspaceHost {
    shared: Arc<Mutex<Shared>>,
    pending: Vec<PendingDrain>,
    workers: Vec<JoinHandle<()>>,
    changed: Option<ChangeSignal>,
    router: Arc<Mutex<InputRouter>>,
    router_timer: Arc<(Mutex<RouterTimerState>, Condvar)>,
    router_timer_started: bool,
    copy_mode: crate::client::CopyMode,
    viewports: BTreeMap<ClientId, (u16, u16, u64)>,
    resize_order: u64,
    killed: bool,
    zor: Option<PathBuf>,
    scrollback: usize,
    capture_bytes: usize,
    resources: crate::config::ResourceLimits,
    default_command: Vec<String>,
    external_socket: Option<PathBuf>,
    external_cwd: PathBuf,
    external_processes: ExternalProcesses,
    external_workers: Vec<JoinHandle<()>>,
}

impl WorkspaceHost {
    pub fn shared(
        command: Vec<String>,
        scrollback: usize,
        zor: Option<PathBuf>,
    ) -> anyhow::Result<(WorkspaceSession, WorkspaceControl)> {
        let inner = Arc::new(Mutex::new(Self::spawn(command, scrollback, zor)?));
        Ok((
            WorkspaceSession {
                inner: Arc::clone(&inner),
            },
            WorkspaceControl { inner },
        ))
    }
    pub fn spawn(
        command: Vec<String>,
        scrollback: usize,
        zor: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let zor = zor.and_then(|path| {
            if let Some(resolved) = resolve_zor_path(&path, std::env::var_os("PATH").as_deref()) {
                Some(resolved)
            } else {
                tracing::warn!(path = %path.display(), "configured zor executable is unavailable; spawning bare panes");
                None
            }
        });
        let mut host = Self {
            shared: Arc::new(Mutex::new(Shared::default())),
            pending: Vec::new(),
            workers: Vec::new(),
            changed: None,
            router: Arc::new(Mutex::new(InputRouter::new(0x01, 40))),
            router_timer: Arc::new((Mutex::new(RouterTimerState::default()), Condvar::new())),
            router_timer_started: false,
            copy_mode: crate::client::CopyMode::default(),
            viewports: BTreeMap::new(),
            resize_order: 0,
            killed: false,
            zor,
            scrollback,
            capture_bytes: crate::config::HistoryLimits::default().capture_bytes,
            resources: crate::config::ResourceLimits::default(),
            default_command: command.clone(),
            external_socket: None,
            external_cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            external_processes: Arc::new(Mutex::new(BTreeMap::new())),
            external_workers: Vec::new(),
        };
        host.add_initial_pane(command)?;
        Ok(host)
    }

    pub fn add_pane(&mut self, command: Vec<String>) -> anyhow::Result<PaneId> {
        self.add_pane_with_axis(command, crate::state::Axis::Horizontal)
    }

    fn set_spawn_metadata(&self, pane: PaneId, command: Vec<String>, cwd: Option<PathBuf>) {
        if let Some(runtime) = lock(&self.shared).panes.get(&pane).cloned() {
            let mut runtime = lock(&runtime);
            runtime.command = command;
            if let Some(cwd) = cwd {
                runtime.cwd = cwd;
            }
        }
    }

    fn add_pane_with_axis(
        &mut self,
        command: Vec<String>,
        axis: crate::state::Axis,
    ) -> anyhow::Result<PaneId> {
        if self.killed {
            anyhow::bail!("workspace is shutting down");
        }
        let shared = lock(&self.shared);
        let new_cells = usize::from(DEFAULT_ROWS).saturating_mul(usize::from(DEFAULT_COLS));
        let current_cells = shared
            .state
            .panes()
            .values()
            .fold(0usize, |total, pane| total.saturating_add(pane.cells.len()));
        if shared.state.panes().len() >= self.resources.max_panes {
            anyhow::bail!("configured pane limit reached");
        }
        let new_pane_units = std::mem::size_of::<PaneView>()
            .saturating_add(new_cells.saturating_mul(std::mem::size_of::<crate::state::Cell>()));
        if current_cells.saturating_add(new_cells) > self.resources.max_total_cells
            || shared
                .state
                .recompute_resource_units()
                .saturating_add(new_pane_units)
                > self.resources.max_units
        {
            anyhow::bail!("configured workspace resource limit reached");
        }
        drop(shared);
        let row_units = history_row_units();
        let reserved = lock(&self.shared)
            .panes
            .values()
            .map(|pane| lock(pane).history_reserved_units)
            .sum::<usize>();
        let history_limit = self.scrollback.min(
            self.resources
                .max_units
                .saturating_sub(reserved)
                .saturating_sub(worst_case_state_units())
                / row_units.max(1),
        );
        let id = {
            let shared = lock(&self.shared);
            PaneId(
                shared
                    .panes
                    .keys()
                    .next_back()
                    .map_or(1, |id| id.0.saturating_add(1)),
            )
        };
        let argv = wrapped_command(&command, self.zor.as_deref());
        let terminal = ServerTerminal::new(DEFAULT_ROWS, DEFAULT_COLS, history_limit);
        let (pty, receiver) = Pty::spawn(DEFAULT_ROWS, DEFAULT_COLS, &argv, "xterm-256color")?;
        let pane = PaneView::from_vt100(
            terminal.snapshot().screen(),
            String::new(),
            AgentStatus::default(),
            0,
        )
        .map_err(|_| anyhow::anyhow!("initial pane exceeds workspace bounds"))?;
        let runtime = Arc::new(Mutex::new(PaneRuntime {
            pty: Some(pty),
            terminal,
            alive: true,
            exit_code: None,
            close_event_published: false,
            last_report: None,
            history_limit,
            history_reserved_units: history_limit.saturating_mul(row_units),
            last_bell_count: 0,
            last_clipboard: String::new(),
            last_output_event_ms: None,
            command: command.clone(),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            geometry: Rect::default(),
        }));
        {
            let mut shared = lock(&self.shared);
            shared
                .state
                .insert_pane(id, pane)
                .map_err(|_| anyhow::anyhow!("pane limit reached"))?;
            shared.panes.insert(id, runtime);
            if let Some(active) = shared.state.active_tab() {
                let mut tabs = shared.state.tabs().to_vec();
                if let Some(tab) = tabs.iter_mut().find(|tab| tab.id == active)
                    && let Some(ratio) = NonZeroU16::new(crate::state::RATIO_SCALE / 2)
                    && tab.layout.split(tab.focused, id, axis, ratio).is_ok()
                {
                    tab.focused = id;
                    let _ = shared.state.replace_tabs(tabs, Some(active));
                }
            } else {
                let tab_id = TabId(
                    shared
                        .state
                        .tabs()
                        .iter()
                        .map(|tab| tab.id.0)
                        .max()
                        .unwrap_or(0)
                        .saturating_add(1),
                );
                let tab = Tab {
                    id: tab_id,
                    name: format!("tab-{}", tab_id.0),
                    layout: LayoutTree::new(id),
                    focused: id,
                    zoomed: None,
                };
                let _ = shared.state.replace_tabs(vec![tab], Some(tab_id));
            }
        }
        let admitted = {
            let shared = lock(&self.shared);
            state_within_resources(&shared.state, &self.resources)
                && total_resource_units(&shared, &shared.state) <= self.resources.max_units
        };
        if !admitted {
            let runtime = {
                let mut shared = lock(&self.shared);
                let runtime = shared.panes.get(&id).cloned();
                remove_exited_pane(&mut shared, id, false);
                runtime
            };
            if let Some(runtime) = runtime {
                let mut runtime = lock(&runtime);
                if let Some(mut pty) = runtime.pty.take() {
                    let _ = pty.shutdown_process_group(std::time::Duration::ZERO);
                    pty.shutdown();
                }
            }
            anyhow::bail!("configured workspace resource limit reached");
        }
        self.pending.push(PendingDrain { pane: id, receiver });
        if self.changed.is_none() && lock(&self.shared).event_sink.is_none() {
            self.start_pending();
        }
        self.apply_geometry();
        if let Some(changed) = &self.changed {
            changed.pulse();
        }
        Ok(id)
    }

    fn add_initial_pane(&mut self, command: Vec<String>) -> anyhow::Result<()> {
        let id = self.add_pane(command)?;
        let tab = Tab {
            id: TabId(1),
            name: "main".into(),
            layout: LayoutTree::new(id),
            focused: id,
            zoomed: None,
        };
        lock(&self.shared)
            .state
            .replace_tabs(vec![tab], Some(TabId(1)))
            .map_err(|_| anyhow::anyhow!("invalid initial layout"))
    }

    fn start_pending(&mut self) {
        let mut active = Vec::with_capacity(self.workers.len());
        for worker in self.workers.drain(..) {
            if worker.is_finished() {
                let _ = worker.join();
            } else {
                active.push(worker);
            }
        }
        self.workers = active;
        for mut pending in self.pending.drain(..) {
            let shared = self.shared.clone();
            self.workers.push(std::thread::spawn(move || {
                while let Some(chunk) = pending.receiver.blocking_recv() {
                    process_chunk(&shared, pending.pane, &chunk);
                }
                finish_pane(&shared, pending.pane);
            }));
        }
    }

    #[must_use]
    pub fn capture(&self, pane: PaneId, scrollback: usize) -> Option<String> {
        self.capture_with_attrs(pane, scrollback, false, self.capture_bytes)
    }

    #[must_use]
    pub fn capture_with_attrs(
        &self,
        pane: PaneId,
        scrollback: usize,
        attrs: bool,
        max_bytes: usize,
    ) -> Option<String> {
        let runtime = { lock(&self.shared).panes.get(&pane).cloned()? };
        let mut runtime = lock(&runtime);
        let columns = usize::from(
            runtime
                .terminal
                .with_scrollback_screen(0, |screen| screen.size().1),
        )
        .max(1);
        let bytes_per_row = if attrs {
            columns.saturating_mul(128)
        } else {
            columns
                .saturating_mul(crate::state::MAX_CELL_TEXT_BYTES)
                .saturating_add(1)
        };
        let bounded_rows = max_bytes.saturating_div(bytes_per_row).saturating_add(1);
        let rows = scrollback.min(runtime.history_limit).min(bounded_rows);
        let mut output = runtime.terminal.with_scrollback_screen(rows, |screen| {
            if attrs {
                String::from_utf8_lossy(&screen.contents_formatted()).into_owned()
            } else {
                screen.contents()
            }
        });
        if output.len() > max_bytes {
            output.truncate(output.floor_char_boundary(max_bytes));
        }
        Some(output)
    }

    fn focused_runtime(&self) -> Option<Arc<Mutex<PaneRuntime>>> {
        let shared = lock(&self.shared);
        let focused = focused_pane_id(&shared)?;
        shared.panes.get(&focused).cloned()
    }

    fn route_input(&mut self, bytes: &[u8]) {
        if self.killed {
            return;
        }
        if let Some(remainder) = self.route_copy_input(bytes) {
            if !remainder.is_empty() {
                self.route_input(&remainder);
            }
            return;
        }
        let now = monotonic_ms();
        let mut forwarded = Vec::new();
        for (offset, byte) in bytes.iter().enumerate() {
            if offset > 0
                && let Some(suffix) = bytes.get(offset..)
                && let Some(remainder) = self.route_copy_input(suffix)
            {
                self.flush_routed_input(&mut forwarded);
                if !remainder.is_empty() {
                    self.route_input(&remainder);
                }
                break;
            }
            let actions = lock(&self.router).feed(std::slice::from_ref(byte), now);
            for action in actions {
                if !matches!(&action, Action::Forward(_)) {
                    self.flush_routed_input(&mut forwarded);
                }
                match action {
                    Action::Forward(bytes) => match self.route_copy_input(&bytes) {
                        Some(remainder) if !remainder.is_empty() => forwarded.extend(remainder),
                        Some(_) => {}
                        None => forwarded.extend(bytes),
                    },
                    Action::Command(Command::Focus(direction)) => {
                        if let Some(pane) = self.focus(direction) {
                            self.publish_local_event(crate::control::Event::PaneFocused {
                                id: 0,
                                pane: pane.0,
                            });
                        }
                    }
                    Action::Command(Command::Close) => {
                        if let Some((pane, status)) = self.close_focused(false) {
                            self.publish_local_event(crate::control::Event::PaneClosed {
                                id: 0,
                                pane: pane.0,
                                exit_status: status,
                            });
                        }
                    }
                    Action::Command(Command::SplitHorizontal) => {
                        if let Ok(pane) = self.add_pane_with_axis(
                            self.default_command.clone(),
                            crate::state::Axis::Horizontal,
                        ) {
                            self.publish_local_event(crate::control::Event::PaneOpened {
                                id: 0,
                                pane: pane.0,
                                command: self.default_command.clone(),
                            });
                            self.start_pending();
                        }
                    }
                    Action::Command(Command::SplitVertical) => {
                        if let Ok(pane) = self.add_pane_with_axis(
                            self.default_command.clone(),
                            crate::state::Axis::Vertical,
                        ) {
                            self.publish_local_event(crate::control::Event::PaneOpened {
                                id: 0,
                                pane: pane.0,
                                command: self.default_command.clone(),
                            });
                            self.start_pending();
                        }
                    }
                    Action::Command(Command::NewPane) => {
                        if let Ok(pane) = self.add_pane(self.default_command.clone()) {
                            self.publish_local_event(crate::control::Event::PaneOpened {
                                id: 0,
                                pane: pane.0,
                                command: self.default_command.clone(),
                            });
                            self.start_pending();
                        }
                    }
                    Action::Command(Command::NewTab) => {
                        if let Ok(change) =
                            tab_action(self, crate::control::TabAction::New { name: None })
                        {
                            self.publish_tab_change(change, 0);
                            self.start_pending();
                        }
                        self.apply_geometry();
                    }
                    Action::Command(Command::NextTab) => {
                        if let Ok(change) = tab_action(self, crate::control::TabAction::Next) {
                            self.publish_tab_change(change, 0);
                        }
                        self.apply_geometry();
                    }
                    Action::Command(Command::PreviousTab) => {
                        if let Ok(change) = tab_action(self, crate::control::TabAction::Previous) {
                            self.publish_tab_change(change, 0);
                        }
                        self.apply_geometry();
                    }
                    Action::Command(Command::Zoom) => self.toggle_zoom(),
                    Action::Command(Command::CopyMode) => {
                        let mut shared = lock(&self.shared);
                        if let Some((focused, pane)) = shared
                            .state
                            .active_tab()
                            .and_then(|active| {
                                shared.state.tabs().iter().find(|tab| tab.id == active)
                            })
                            .and_then(|tab| {
                                shared
                                    .state
                                    .pane(tab.focused)
                                    .cloned()
                                    .map(|pane| (tab.focused, pane))
                            })
                        {
                            self.copy_mode.enter(&pane);
                            self.copy_mode.sync(&mut shared.state, focused);
                            // `sync` establishes the pane target after entry.
                            let _ = self.copy_mode.key(&[], &mut shared.state, focused);
                            drop(shared);
                            if let Some(changed) = &self.changed {
                                changed.pulse();
                            }
                        }
                    }
                    Action::Command(Command::External(argv)) => self.run_external_binding(argv),
                    Action::Command(Command::WorkspacePicker) => {
                        self.set_transient_status(
                            "picker",
                            "workspace picker requires a named manager attachment",
                        );
                    }
                    Action::Command(Command::Help) => self.set_transient_status(
                        "help",
                        "prefix: |/- split, hjkl focus, t/n/p tabs, x close, z zoom, [ copy",
                    ),
                    Action::Command(Command::Detach) => self.set_transient_status(
                        "error",
                        "detach is handled by the client escape sequence",
                    ),
                    Action::Mouse(mouse) => self.route_mouse(mouse),
                }
            }
        }
        self.flush_routed_input(&mut forwarded);
        self.schedule_router_timeout();
    }

    fn flush_routed_input(&mut self, forwarded: &mut Vec<u8>) {
        if !forwarded.is_empty() {
            self.write_focused(forwarded);
            forwarded.clear();
        }
    }

    fn route_copy_input(&mut self, bytes: &[u8]) -> Option<Vec<u8>> {
        if !self.copy_mode.active() {
            return None;
        }
        let mut shared = lock(&self.shared);
        let prior_state = shared.state.clone();
        let focused = shared.state.active_tab().and_then(|active| {
            shared
                .state
                .tabs()
                .iter()
                .find(|tab| tab.id == active)
                .map(|tab| tab.focused)
        });
        let Some(focused) = focused else {
            self.copy_mode.reset_synced(&mut shared.state);
            return Some(Vec::new());
        };
        let remainder = if !self.copy_mode.targets(focused) {
            self.copy_mode.reset_synced(&mut shared.state);
            Vec::new()
        } else {
            self.copy_mode
                .key_with_remainder(bytes, &mut shared.state, focused)
                .1
        };
        if total_resource_units(&shared, &shared.state) > self.resources.max_units {
            shared.state = prior_state;
            self.copy_mode.reset_synced(&mut shared.state);
        }
        drop(shared);
        self.refresh_pane_view(focused);
        if let Some(changed) = &self.changed {
            changed.pulse();
        }
        Some(remainder)
    }

    fn set_transient_status(&mut self, segment: &str, text: &str) {
        let admitted = {
            let mut shared = lock(&self.shared);
            let mut candidate = shared.state.clone();
            candidate
                .update_metadata(|metadata| {
                    metadata.status.insert(segment.to_owned(), text.to_owned());
                })
                .is_ok()
                && commit_resource_candidate(&mut shared, candidate).is_ok()
        };
        if admitted && let Some(changed) = &self.changed {
            changed.pulse();
        }
    }

    fn publish_local_event(&self, event: crate::control::Event) {
        if let Some(sink) = lock(&self.shared).event_sink.clone() {
            sink.publish(event);
        }
    }

    fn publish_tab_change(&self, change: TabMutation, id: u64) {
        self.publish_local_event(if change.opened {
            crate::control::Event::PaneOpened {
                id,
                pane: change.pane.0,
                command: self.default_command.clone(),
            }
        } else {
            crate::control::Event::PaneFocused {
                id,
                pane: change.pane.0,
            }
        });
    }

    fn flush_router_timeout(&mut self) {
        let actions = lock(&self.router).flush_timeout(monotonic_ms());
        for action in actions {
            if let Action::Forward(bytes) = action {
                self.write_focused(&bytes);
            }
        }
    }

    fn reap_exited_panes(&mut self) {
        // Drain workers own completion. EOF is observed only after all earlier chunks have been
        // processed, so snapshotting must never race them by reaping/removing a runtime here.
    }

    fn schedule_router_timeout(&mut self) {
        let (state, wake) = &*self.router_timer;
        lock(state).deadline = lock(&self.router)
            .has_pending_timeout()
            .then(|| std::time::Instant::now() + std::time::Duration::from_millis(41));
        wake.notify_one();
    }

    fn start_router_timer(&mut self) {
        if self.router_timer_started {
            return;
        }
        self.router_timer_started = true;
        let control = Arc::clone(&self.router_timer);
        let router = Arc::clone(&self.router);
        let shared = Arc::clone(&self.shared);
        let changed = self.changed.clone();
        self.workers.push(std::thread::spawn(move || {
            loop {
                let (state, wake) = &*control;
                let mut state = lock(state);
                while state.deadline.is_none() && !state.shutdown {
                    state = wake
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                if state.shutdown {
                    break;
                }
                let deadline = state.deadline.unwrap_or_else(std::time::Instant::now);
                let now = std::time::Instant::now();
                if now < deadline {
                    let (next, timeout) = wake
                        .wait_timeout(state, deadline.saturating_duration_since(now))
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state = next;
                    if !timeout.timed_out() {
                        continue;
                    }
                }
                state.deadline = None;
                drop(state);
                let actions = lock(&router).flush_timeout(monotonic_ms());
                let mut forwarded = false;
                for action in actions {
                    if let Action::Forward(bytes) = action {
                        write_shared_focused(&shared, &bytes);
                        forwarded = true;
                    }
                }
                if forwarded && let Some(changed) = &changed {
                    changed.pulse();
                }
            }
        }));
    }

    fn run_external_binding(&mut self, argv: Vec<String>) {
        use std::os::unix::process::CommandExt as _;
        let Some((program, arguments)) = argv.split_first() else {
            return;
        };
        let mut active = Vec::new();
        for worker in self.external_workers.drain(..) {
            if worker.is_finished() {
                let _ = worker.join();
            } else {
                active.push(worker);
            }
        }
        self.external_workers = active;
        if lock(&self.external_processes).len() >= 16 {
            self.set_transient_status("error", "external binding process limit reached");
            return;
        }
        let shared = lock(&self.shared);
        let focused = focused_pane_id(&shared);
        let pane = focused.map_or(0, |pane| pane.0);
        let cwd = focused
            .and_then(|pane| shared.panes.get(&pane))
            .map(|runtime| lock(runtime).cwd.clone())
            .unwrap_or_else(|| self.external_cwd.clone());
        drop(shared);
        let mut command = std::process::Command::new(program);
        command.args(arguments).env_clear();
        for (key, value) in std::env::vars_os() {
            let text = key.to_string_lossy();
            if !text.starts_with("FUX_") && !text.starts_with("KOH_") {
                command.env(key, value);
            }
        }
        command
            .env("FUX_PANE", pane.to_string())
            .env("FUX_CWD", cwd)
            .process_group(0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Some(socket) = &self.external_socket {
            command.env("FUX_SOCKET", socket);
        }
        if let Ok(child) = command.spawn() {
            let processes = Arc::clone(&self.external_processes);
            let pgid = i32::try_from(child.id()).ok();
            if let Some(pgid) = pgid {
                lock(&processes).insert(pgid, Arc::new(Mutex::new(Some(child))));
            } else {
                return;
            }
            self.external_workers.push(std::thread::spawn(move || {
                if let Some(pgid) = pgid {
                    loop {
                        let exited = {
                            let processes = lock(&processes);
                            let Some(process) = processes.get(&pgid) else {
                                return;
                            };
                            let mut child = lock(process);
                            match child.as_mut() {
                                None => true,
                                Some(process) => {
                                    let exited = process.try_wait().ok().flatten().is_some();
                                    if exited {
                                        *child = None;
                                    }
                                    exited
                                }
                            }
                        };
                        if exited {
                            lock(&processes).remove(&pgid);
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
            }));
        }
    }

    fn write_focused(&mut self, bytes: &[u8]) {
        if let Some(runtime) = self.focused_runtime()
            && let Some(pty) = lock(&runtime).pty.as_ref()
        {
            for chunk in bytes.chunks(4096) {
                if pty.write_input(chunk).is_err() {
                    self.set_transient_status("error", "pane input queue is full");
                    break;
                }
            }
        }
    }

    fn focus(&mut self, direction: crate::state::Direction) -> Option<PaneId> {
        let mut shared = lock(&self.shared);
        let active = shared.state.active_tab()?;
        let mut tabs = shared.state.tabs().to_vec();
        let tab = tabs.iter_mut().find(|tab| tab.id == active)?;
        if let Some(next) = tab.layout.neighbour(
            tab.focused,
            direction,
            Rect {
                x: 0,
                y: 0,
                width: 1000,
                height: 1000,
            },
        ) {
            tab.focused = next;
            let _ = shared.state.replace_tabs(tabs, Some(active));
            drop(shared);
            if let Some(changed) = &self.changed {
                changed.pulse();
            }
            return Some(next);
        }
        None
    }

    fn close_focused(&mut self, wait_for_exit: bool) -> Option<(PaneId, Option<i32>)> {
        let popup = {
            lock(&self.shared)
                .state
                .popups()
                .iter()
                .max_by_key(|popup| popup.z_index)
                .map(|popup| popup.pane)
        };
        if let Some(popup) = popup {
            return self.close_popup(popup, wait_for_exit);
        }
        let mut shared = lock(&self.shared);
        let active = shared.state.active_tab()?;
        let mut tabs = shared.state.tabs().to_vec();
        let index = tabs.iter().position(|tab| tab.id == active)?;
        let pane_id = tabs.get(index).map(|tab| tab.focused)?;
        let next = tabs
            .get_mut(index)
            .and_then(|tab| tab.layout.close(pane_id).ok())
            .flatten();
        if let Some(tab) = tabs.get_mut(index)
            && let Some(next) = next
        {
            tab.focused = next;
            tab.zoomed = tab.zoomed.filter(|id| *id != pane_id);
        }
        if next.is_none() {
            tabs.remove(index);
        }
        let next_active = if tabs.is_empty() {
            None
        } else if tabs.iter().any(|tab| tab.id == active) {
            Some(active)
        } else {
            tabs.first().map(|tab| tab.id)
        };
        if shared.state.replace_tabs(tabs, next_active).is_err() {
            return None;
        }
        let _ = shared.state.remove_pane(pane_id);
        let mut exit_code = None;
        let mut deferred = None;
        if let Some(runtime) = shared.panes.remove(&pane_id) {
            let mut runtime = lock(&runtime);
            if wait_for_exit {
                if let Some(pty) = runtime.pty.as_mut()
                    && let Ok(Some(status)) =
                        pty.shutdown_process_group(std::time::Duration::from_millis(1_500))
                {
                    exit_code = Some(status.exit_code());
                }
            } else if let Some(pty) = runtime.pty.take() {
                deferred = Some(pty);
            }
        }
        if shared.panes.is_empty() {
            let _ = shared.state.update_metadata(|metadata| {
                metadata.exit_code = Some(exit_code.unwrap_or(0));
            });
        }
        drop(shared);
        if let Some(mut pty) = deferred {
            let _ = pty.terminate_process_group(false);
            self.workers.push(std::thread::spawn(move || {
                let _ = pty.shutdown_process_group(std::time::Duration::from_millis(100));
                pty.shutdown();
            }));
        }
        self.apply_geometry();
        if let Some(changed) = &self.changed {
            changed.pulse();
        }
        Some((
            pane_id,
            exit_code.map(|code| i32::try_from(code).unwrap_or(i32::MAX)),
        ))
    }

    fn close_popup(
        &mut self,
        pane_id: PaneId,
        wait_for_exit: bool,
    ) -> Option<(PaneId, Option<i32>)> {
        let mut shared = lock(&self.shared);
        if !shared
            .state
            .popups()
            .iter()
            .any(|popup| popup.pane == pane_id)
        {
            return None;
        }
        let runtime = shared.panes.remove(&pane_id)?;
        let mut exit_status = None;
        let mut deferred = None;
        {
            let mut runtime = lock(&runtime);
            if wait_for_exit {
                if let Some(pty) = runtime.pty.as_mut()
                    && let Ok(Some(status)) =
                        pty.shutdown_process_group(std::time::Duration::from_millis(1_500))
                {
                    exit_status = Some(i32::try_from(status.exit_code()).unwrap_or(i32::MAX));
                }
            } else if let Some(pty) = runtime.pty.take() {
                deferred = Some(pty);
            }
        }
        let popups = shared
            .state
            .popups()
            .iter()
            .filter(|popup| popup.pane != pane_id)
            .cloned()
            .collect();
        let _ = shared.state.replace_popups(popups);
        let _ = shared.state.remove_pane(pane_id);
        drop(shared);
        if let Some(mut pty) = deferred {
            let _ = pty.terminate_process_group(false);
            self.workers.push(std::thread::spawn(move || {
                let _ = pty.shutdown_process_group(std::time::Duration::from_millis(100));
                pty.shutdown();
            }));
        }
        self.apply_geometry();
        if let Some(changed) = &self.changed {
            changed.pulse();
        }
        Some((pane_id, exit_status))
    }

    fn discard_unreferenced_pane(&mut self, pane_id: PaneId) {
        let runtime = {
            let mut shared = lock(&self.shared);
            let _ = shared.state.remove_pane(pane_id);
            shared.panes.remove(&pane_id)
        };
        if let Some(runtime) = runtime {
            let mut runtime = lock(&runtime);
            if let Some(pty) = runtime.pty.as_mut() {
                let _ = pty.kill();
                pty.kill_hard();
            }
        }
    }

    fn toggle_zoom(&mut self) {
        let mut shared = lock(&self.shared);
        let Some(active) = shared.state.active_tab() else {
            return;
        };
        let mut tabs = shared.state.tabs().to_vec();
        if let Some(tab) = tabs.iter_mut().find(|tab| tab.id == active) {
            tab.zoomed = if tab.zoomed == Some(tab.focused) {
                None
            } else {
                Some(tab.focused)
            };
            let _ = shared.state.replace_tabs(tabs, Some(active));
        }
        drop(shared);
        self.apply_geometry();
        if let Some(changed) = &self.changed {
            changed.pulse();
        }
    }

    fn route_mouse(&mut self, mouse: MouseEvent) {
        let Some((rows, columns, _)) = self.viewports.values().max_by_key(|value| value.2).copied()
        else {
            return;
        };
        let mut shared = lock(&self.shared);
        if let Some(popup) = shared
            .state
            .popups()
            .iter()
            .max_by_key(|popup| popup.z_index)
            .cloned()
        {
            let body_height = rows.saturating_sub(1);
            let rect = Rect {
                x: columns.saturating_sub(popup.width) / 2,
                y: body_height.saturating_sub(popup.height) / 2,
                width: popup.width.min(columns),
                height: popup.height.min(body_height),
            };
            let content = Rect {
                x: rect.x.saturating_add(1),
                y: rect.y.saturating_add(1),
                width: rect.width.saturating_sub(2),
                height: rect.height.saturating_sub(2),
            };
            let x = mouse.column.saturating_sub(1);
            let y = mouse.row.saturating_sub(1);
            let inside = x >= content.x
                && x < content.x.saturating_add(content.width)
                && y >= content.y
                && y < content.y.saturating_add(content.height);
            let runtime = inside
                .then(|| shared.panes.get(&popup.pane).cloned())
                .flatten();
            let modes = shared.state.pane(popup.pane).map(|pane| pane.modes);
            drop(shared);
            if let Some(runtime) = runtime
                && let Some(pty) = lock(&runtime).pty.as_ref()
                && let Some(bytes) = modes.and_then(|modes| {
                    encode_mouse(
                        mouse,
                        x.saturating_sub(rect.x),
                        y.saturating_sub(rect.y),
                        modes,
                    )
                })
            {
                let _ = pty.write_input(&bytes);
            }
            return;
        }
        let Some(active) = shared.state.active_tab() else {
            return;
        };
        let mut tabs = shared.state.tabs().to_vec();
        let Some(tab) = tabs.iter_mut().find(|tab| tab.id == active) else {
            return;
        };
        let geometry = tab
            .layout
            .geometry(Rect {
                x: 0,
                y: 0,
                width: columns,
                height: rows.saturating_sub(1),
            })
            .unwrap_or_default();
        let x = mouse.column.saturating_sub(1);
        let y = mouse.row.saturating_sub(1);
        let Some((target, rect)) = geometry.into_iter().find(|(_, rect)| {
            x >= rect.x
                && x < rect.x.saturating_add(rect.width)
                && y >= rect.y
                && y < rect.y.saturating_add(rect.height)
        }) else {
            return;
        };
        if mouse.shift() {
            if let Some(pane) = shared.state.pane(target).cloned() {
                if !self.copy_mode.active() {
                    self.copy_mode.enter(&pane);
                }
                self.copy_mode.bind_target(target);
                let _ = self.copy_mode.shift_drag(
                    y.saturating_sub(rect.y.saturating_add(1)),
                    x.saturating_sub(rect.x.saturating_add(1)),
                    mouse.release,
                    &pane,
                );
                self.copy_mode.sync(&mut shared.state, target);
            }
            drop(shared);
            if let Some(changed) = &self.changed {
                changed.pulse();
            }
            return;
        }
        if mouse.code & 3 == 0 {
            let focus_changed = tab.focused != target;
            tab.focused = target;
            let _ = shared.state.replace_tabs(tabs, Some(active));
            if let Some(changed) = &self.changed {
                changed.pulse();
            }
            if focus_changed && let Some(sink) = shared.event_sink.clone() {
                sink.publish(crate::control::Event::PaneFocused {
                    id: 0,
                    pane: target.0,
                });
            }
        }
        if mouse.wheel()
            && shared
                .state
                .pane(target)
                .is_some_and(|pane| pane.modes.mouse_mode == MouseMode::None)
        {
            let up = mouse.code & 1 == 0;
            let _ = shared.state.update_pane(target, |pane| {
                pane.viewport_offset = if up {
                    pane.viewport_offset.saturating_add(3)
                } else {
                    pane.viewport_offset.saturating_sub(3)
                }
            });
            drop(shared);
            self.refresh_pane_view(target);
            if let Some(changed) = &self.changed {
                changed.pulse();
            }
            return;
        }
        let claimed = shared
            .state
            .pane(target)
            .is_some_and(|pane| pane.modes.mouse_mode != MouseMode::None);
        let runtime = shared.panes.get(&target).cloned();
        let modes = shared.state.pane(target).map(|pane| pane.modes);
        drop(shared);
        let inside_content = x > rect.x
            && x < rect.x.saturating_add(rect.width).saturating_sub(1)
            && y > rect.y
            && y < rect.y.saturating_add(rect.height).saturating_sub(1);
        if claimed
            && inside_content
            && let Some(runtime) = runtime
            && let Some(pty) = lock(&runtime).pty.as_ref()
            && let Some(bytes) = modes.and_then(|modes| {
                encode_mouse(
                    mouse,
                    x.saturating_sub(rect.x),
                    y.saturating_sub(rect.y),
                    modes,
                )
            })
        {
            let _ = pty.write_input(&bytes);
        }
    }

    fn apply_geometry(&mut self) {
        let Some((rows, columns, _)) = self.viewports.values().max_by_key(|value| value.2).copied()
        else {
            return;
        };
        let mut shared = lock(&self.shared);
        let Some(active) = shared.state.active_tab() else {
            return;
        };
        let Some(tab) = shared.state.tabs().iter().find(|tab| tab.id == active) else {
            return;
        };
        let area = Rect {
            x: 0,
            y: 0,
            width: columns,
            height: rows.saturating_sub(1),
        };
        let geometry = tab.layout.geometry(area).unwrap_or_default();
        let mut panes: Vec<_> = geometry
            .into_iter()
            .filter_map(|(id, rect)| shared.panes.get(&id).cloned().map(|pane| (id, pane, rect)))
            .collect();
        panes.extend(shared.state.popups().iter().filter_map(|popup| {
            let body_height = rows.saturating_sub(1);
            shared.panes.get(&popup.pane).cloned().map(|pane| {
                (
                    popup.pane,
                    pane,
                    Rect {
                        x: columns.saturating_sub(popup.width) / 2,
                        y: body_height.saturating_sub(popup.height) / 2,
                        width: popup.width.min(columns),
                        height: popup.height.min(body_height),
                    },
                )
            })
        }));
        let requested_cells = panes.iter().fold(0usize, |total, (_, _, rect)| {
            total.saturating_add(
                usize::from(rect.height.saturating_sub(2).max(2))
                    .saturating_mul(usize::from(rect.width.saturating_sub(2).max(2))),
            )
        });
        if requested_cells > self.resources.max_total_cells
            || requested_cells > self.resources.max_units
        {
            drop(shared);
            self.set_transient_status("error", "configured terminal cell budget reached");
            return;
        }
        let mut candidate = shared.state.clone();
        let geometry_valid = panes.iter().all(|(id, _, rect)| {
            let pane_rows = rect.height.saturating_sub(2).max(2);
            let pane_cols = rect.width.saturating_sub(2).max(2);
            candidate
                .update_pane(*id, |pane| {
                    pane.rows = pane_rows;
                    pane.columns = pane_cols;
                    pane.cells = vec![
                        crate::state::Cell::default();
                        usize::from(pane_rows) * usize::from(pane_cols)
                    ];
                    pane.wrapped_rows = vec![false; usize::from(pane_rows)];
                })
                .is_ok()
        });
        let projected = total_resource_units(&shared, &candidate)
            .saturating_add(requested_cells.saturating_mul(crate::state::MAX_CELL_TEXT_BYTES));
        if !geometry_valid || projected > self.resources.max_units {
            drop(shared);
            self.set_transient_status("error", "configured terminal byte budget reached");
            return;
        }
        let current = total_resource_units(&shared, &shared.state);
        shared.resource_reservation = projected.saturating_sub(current);
        drop(shared);
        let mut refreshed = Vec::new();
        for (id, runtime, rect) in panes {
            let pane_rows = rect.height.saturating_sub(2).max(2);
            let pane_cols = rect.width.saturating_sub(2).max(2);
            let mut runtime = lock(&runtime);
            runtime.geometry = rect;
            if let Some(pty) = runtime.pty.as_ref() {
                let _ = pty.resize(pane_rows, pane_cols);
            }
            runtime.terminal.resize(pane_rows, pane_cols);
            let snapshot = runtime.terminal.snapshot();
            if let Ok(view) = PaneView::from_vt100(
                snapshot.screen(),
                snapshot.title().to_owned(),
                AgentStatus::default(),
                0,
            ) {
                refreshed.push((id, view));
            }
        }
        let mut shared = lock(&self.shared);
        let mut candidate = shared.state.clone();
        for (id, mut view) in refreshed {
            if let Some(old) = candidate.pane(id) {
                view.agent = old.agent.clone();
                view.viewport_offset = old.viewport_offset;
                view.copy = old.copy;
            }
            let _ = candidate.update_pane(id, |pane| *pane = view);
        }
        // Releasing our reservation and committing happen under the same lock. Output arriving
        // during the PTY calls therefore either consumes the unreserved budget or is rejected.
        shared.resource_reservation = 0;
        if state_within_resources(&candidate, &shared.resources)
            && total_resource_units(&shared, &candidate) <= shared.resources.max_units
        {
            shared.state = candidate;
        } else {
            drop(shared);
            self.set_transient_status("error", "configured terminal byte budget reached");
            return;
        }
        if let Some(changed) = &self.changed {
            changed.pulse();
        }
    }

    fn refresh_pane_view(&mut self, pane_id: PaneId) {
        let (runtime, offset, agent, copy) = {
            let shared = lock(&self.shared);
            let Some(runtime) = shared.panes.get(&pane_id).cloned() else {
                return;
            };
            let Some(pane) = shared.state.pane(pane_id) else {
                return;
            };
            (runtime, pane.viewport_offset, pane.agent.clone(), pane.copy)
        };
        let view = {
            let runtime = lock(&runtime);
            let snapshot = runtime.terminal.snapshot();
            let mut screen = snapshot.screen().clone();
            screen.set_scrollback(
                usize::try_from(offset)
                    .unwrap_or(usize::MAX)
                    .min(runtime.history_limit),
            );
            PaneView::from_vt100(&screen, snapshot.title().to_owned(), agent, offset)
                .map(|mut view| {
                    view.copy = copy;
                    view
                })
                .ok()
        };
        if let Some(mut view) = view {
            let mut shared = lock(&self.shared);
            if !shared
                .panes
                .get(&pane_id)
                .is_some_and(|current| Arc::ptr_eq(current, &runtime))
            {
                return;
            }
            let Some(current) = shared.state.pane(pane_id) else {
                return;
            };
            // Preserve synchronized fields that may have changed while the terminal snapshot was
            // cloned. Only the rendered cells/title come from the runtime refresh.
            view.agent = current.agent.clone();
            view.viewport_offset = current.viewport_offset;
            view.copy = current.copy;
            let mut candidate = shared.state.clone();
            if candidate.update_pane(pane_id, |pane| *pane = view).is_ok() {
                let _ = commit_resource_candidate(&mut shared, candidate);
            }
        }
    }
}

impl WorkspaceControl {
    pub fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub fn is_empty(&self) -> bool {
        lock(&lock(&self.inner).shared).panes.values().all(|pane| {
            let pane = lock(pane);
            !pane.alive && pane.close_event_published
        })
    }

    pub fn attached_clients(&self) -> usize {
        lock(&self.inner).viewports.len()
    }

    pub fn terminal_exit_code(&self) -> Option<u32> {
        lock(&lock(&self.inner).shared).state.metadata().exit_code
    }

    pub fn configure_bindings(
        &self,
        config: &crate::config::Config,
        socket: PathBuf,
    ) -> anyhow::Result<()> {
        let mut bindings = BTreeMap::new();
        for (key, binding) in &config.bindings {
            let byte = binding_byte(key)
                .ok_or_else(|| anyhow::anyhow!("binding `{key}` must encode one byte"))?;
            let command = match binding {
                crate::config::Binding::Builtin { builtin } => builtin_command(*builtin),
                crate::config::Binding::External { external } => {
                    Command::External(external.argv.clone())
                }
            };
            bindings.insert(byte, command);
        }
        let prefix = binding_byte(&config.prefix)
            .ok_or_else(|| anyhow::anyhow!("prefix must encode one byte"))?;
        let mut host = lock(&self.inner);
        let reserved_history = lock(&host.shared)
            .panes
            .values()
            .map(|pane| lock(pane).history_reserved_units)
            .sum::<usize>();
        let shared = lock(&host.shared);
        if reserved_history.saturating_add(shared.state.recompute_resource_units())
            > config.resources.max_units
            || !state_within_resources(&shared.state, &config.resources)
        {
            anyhow::bail!("current workspace exceeds configured resource limits");
        }
        drop(shared);
        host.scrollback = usize::try_from(config.history.scrollback_lines).unwrap_or(usize::MAX);
        host.capture_bytes = config.history.capture_bytes;
        host.resources = config.resources.clone();
        lock(&host.shared).resources = config.resources.clone();
        host.default_command = config.default_command.argv.clone();
        *lock(&host.router) = InputRouter::with_bindings(prefix, 40, bindings);
        host.external_socket = Some(socket);
        Ok(())
    }

    pub fn reconfigure_bindings(&self, config: &crate::config::Config) -> anyhow::Result<()> {
        let socket = lock(&self.inner)
            .external_socket
            .clone()
            .ok_or_else(|| anyhow::anyhow!("workspace control socket is unavailable"))?;
        self.configure_bindings(config, socket)
    }
    pub fn summary(&self, name: String) -> crate::control::WorkspaceSummary {
        let host = lock(&self.inner);
        let shared = lock(&host.shared);
        let active = shared.state.active_tab();
        let mut tabs: Vec<_> = shared
            .state
            .tabs()
            .iter()
            .enumerate()
            .map(|(index, tab)| crate::control::TabSummary {
                index: u32::try_from(index).unwrap_or(u32::MAX),
                name: tab.name.clone(),
                focused: active == Some(tab.id),
                panes: tab
                    .layout
                    .leaves()
                    .into_iter()
                    .filter_map(|pane_id| {
                        let view = shared.state.pane(pane_id)?;
                        let runtime = shared.panes.get(&pane_id).map(|runtime| lock(runtime))?;
                        Some(crate::control::PaneSummary {
                            id: pane_id.0,
                            command: runtime.command.clone(),
                            pid: runtime.pty.as_ref().and_then(Pty::process_id),
                            cwd: runtime.cwd.clone(),
                            title: view.title.clone(),
                            agent: view.agent.id.clone(),
                            state: control_agent_state(view.agent.state),
                            geometry: crate::control::PaneGeometry {
                                x: runtime.geometry.x,
                                y: runtime.geometry.y,
                                width: runtime.geometry.width,
                                height: runtime.geometry.height,
                            },
                            focused: tab.focused == pane_id,
                        })
                    })
                    .collect(),
            })
            .collect();
        if !shared.state.popups().is_empty() {
            let top = shared
                .state
                .popups()
                .iter()
                .max_by_key(|popup| popup.z_index)
                .map(|popup| popup.pane);
            let panes = shared
                .state
                .popups()
                .iter()
                .filter_map(|popup| {
                    let pane_id = popup.pane;
                    let view = shared.state.pane(pane_id)?;
                    let runtime = shared.panes.get(&pane_id).map(|runtime| lock(runtime))?;
                    Some(crate::control::PaneSummary {
                        id: pane_id.0,
                        command: runtime.command.clone(),
                        pid: runtime.pty.as_ref().and_then(Pty::process_id),
                        cwd: runtime.cwd.clone(),
                        title: view.title.clone(),
                        agent: view.agent.id.clone(),
                        state: control_agent_state(view.agent.state),
                        geometry: crate::control::PaneGeometry {
                            x: runtime.geometry.x,
                            y: runtime.geometry.y,
                            width: runtime.geometry.width,
                            height: runtime.geometry.height,
                        },
                        focused: top == Some(pane_id),
                    })
                })
                .collect();
            tabs.push(crate::control::TabSummary {
                index: u32::try_from(tabs.len()).unwrap_or(u32::MAX),
                name: "popups".to_owned(),
                focused: false,
                panes,
            });
        }
        crate::control::WorkspaceSummary {
            name,
            focused: true,
            tabs,
        }
    }
    pub fn set_event_sink(&self, sink: Arc<dyn WorkspaceEventSink>) {
        lock(&lock(&self.inner).shared).event_sink = Some(sink);
    }

    /// Stops every pane and joins all workspace-owned drain workers.
    pub fn shutdown(&self) {
        lock(&self.inner).shutdown_all();
    }
    pub fn dispatch(
        &self,
        request: crate::control::Request,
    ) -> (crate::control::Reply, Vec<crate::control::Event>) {
        use crate::control::{CommandResult, ErrorCode, Event, FocusTarget, Reply, Request};
        let id = request.id();
        let mut host = lock(&self.inner);
        if host.killed {
            return (
                Reply::Failed {
                    id,
                    error: crate::control::ReplyError {
                        code: ErrorCode::Conflict,
                        message: "workspace is shutting down".to_owned(),
                    },
                },
                Vec::new(),
            );
        }
        let result: anyhow::Result<(CommandResult, Vec<Event>)> = (|| match request {
            Request::New { cwd, argv, env, .. } => {
                let original = if argv.is_empty() {
                    host.default_command.clone()
                } else {
                    argv.clone()
                };
                let command = contextual_command(argv, cwd.as_deref(), env, &host.default_command);
                let pane = host.add_pane(command.clone())?;
                host.set_spawn_metadata(pane, original, cwd);
                Ok((
                    CommandResult::Pane { pane: pane.0 },
                    vec![Event::PaneOpened {
                        id,
                        pane: pane.0,
                        command,
                    }],
                ))
            }
            Request::Split {
                axis,
                target,
                argv,
                env,
                ..
            } => {
                let original = if argv.is_empty() {
                    host.default_command.clone()
                } else {
                    argv.clone()
                };
                let previous_focus = {
                    let shared = lock(&host.shared);
                    shared.state.active_tab().and_then(|active| {
                        shared
                            .state
                            .tabs()
                            .iter()
                            .find(|tab| tab.id == active)
                            .map(|tab| tab.focused)
                    })
                };
                if let Some(target) = target {
                    focus_pane(&mut host, PaneId(target))
                        .ok_or_else(|| anyhow::anyhow!("pane not found"))?;
                }
                let axis = match axis {
                    crate::control::Axis::Horizontal => crate::state::Axis::Horizontal,
                    crate::control::Axis::Vertical => crate::state::Axis::Vertical,
                };
                let command = contextual_command(argv, None, env, &host.default_command);
                let pane = match host.add_pane_with_axis(command.clone(), axis) {
                    Ok(pane) => pane,
                    Err(error) => {
                        if let Some(previous) = previous_focus {
                            let _ = focus_pane(&mut host, previous);
                        }
                        return Err(error);
                    }
                };
                host.set_spawn_metadata(pane, original, None);
                Ok((
                    CommandResult::Pane { pane: pane.0 },
                    vec![Event::PaneOpened {
                        id,
                        pane: pane.0,
                        command,
                    }],
                ))
            }
            Request::Focus { target, .. } => {
                let target = match target {
                    FocusTarget::Pane(pane) => focus_pane(&mut host, PaneId(pane)),
                    FocusTarget::Left => focus_direction(&mut host, crate::state::Direction::Left),
                    FocusTarget::Right => {
                        focus_direction(&mut host, crate::state::Direction::Right)
                    }
                    FocusTarget::Up => focus_direction(&mut host, crate::state::Direction::Up),
                    FocusTarget::Down => focus_direction(&mut host, crate::state::Direction::Down),
                }
                .ok_or_else(|| anyhow::anyhow!("pane not found"))?;
                Ok((
                    CommandResult::Unit,
                    vec![Event::PaneFocused { id, pane: target.0 }],
                ))
            }
            Request::Zoom { pane, .. } => {
                if let Some(pane) = pane {
                    focus_pane(&mut host, PaneId(pane))
                        .ok_or_else(|| anyhow::anyhow!("pane not found"))?;
                }
                host.toggle_zoom();
                Ok((CommandResult::Unit, Vec::new()))
            }
            Request::Kill { pane, .. } => {
                let pane_id = PaneId(pane);
                let popup = lock(&host.shared)
                    .state
                    .popups()
                    .iter()
                    .any(|popup| popup.pane == pane_id);
                let (_, exit_status) = if popup {
                    host.close_popup(pane_id, true)
                } else {
                    focus_pane(&mut host, pane_id)
                        .ok_or_else(|| anyhow::anyhow!("pane not found"))?;
                    host.close_focused(true)
                }
                .ok_or_else(|| anyhow::anyhow!("pane not found"))?;
                Ok((
                    CommandResult::Unit,
                    vec![Event::PaneClosed {
                        id,
                        pane,
                        exit_status,
                    }],
                ))
            }
            Request::SendKeys { pane, keys, .. } => {
                let bytes = crate::control::decode_key_bytes(&keys)?;
                write_pane(&host, PaneId(pane), &bytes)?;
                Ok((CommandResult::Unit, Vec::new()))
            }
            Request::Capture {
                pane,
                scrollback,
                max_bytes,
                attrs,
                ..
            } => {
                let limit = max_bytes.min(host.capture_bytes);
                let text = host
                    .capture_with_attrs(PaneId(pane), scrollback as usize, attrs, limit)
                    .ok_or_else(|| anyhow::anyhow!("pane not found"))?;
                Ok((CommandResult::Capture { text }, Vec::new()))
            }
            Request::SetStatus { segment, text, .. } => {
                let mut shared = lock(&host.shared);
                let adding =
                    !text.is_empty() && !shared.state.metadata().status.contains_key(&segment);
                if adding
                    && shared.state.metadata().status.len() >= host.resources.max_status_segments
                {
                    anyhow::bail!("configured status segment limit reached");
                }
                let mut candidate = shared.state.clone();
                candidate
                    .update_metadata(|metadata| {
                        if text.is_empty() {
                            metadata.status.remove(&segment);
                        } else {
                            metadata.status.insert(segment, text);
                        }
                    })
                    .map_err(|_| anyhow::anyhow!("status limit reached"))?;
                if total_resource_units(&shared, &candidate) > host.resources.max_units {
                    anyhow::bail!("configured workspace resource limit reached");
                }
                shared.state = candidate;
                Ok((CommandResult::Unit, Vec::new()))
            }
            Request::Resize { pane, delta, .. } => {
                let mut shared = lock(&host.shared);
                let active = shared
                    .state
                    .active_tab()
                    .ok_or_else(|| anyhow::anyhow!("no active tab"))?;
                let mut tabs = shared.state.tabs().to_vec();
                let tab = tabs
                    .iter_mut()
                    .find(|tab| tab.id == active)
                    .ok_or_else(|| anyhow::anyhow!("no active tab"))?;
                tab.layout
                    .resize(PaneId(pane), delta)
                    .map_err(|_| anyhow::anyhow!("pane cannot be resized"))?;
                let mut candidate = shared.state.clone();
                candidate
                    .replace_tabs(tabs, Some(active))
                    .map_err(|_| anyhow::anyhow!("invalid resize"))?;
                commit_resource_candidate(&mut shared, candidate)
                    .map_err(|_| anyhow::anyhow!("configured workspace resource limit reached"))?;
                Ok((CommandResult::Unit, Vec::new()))
            }
            Request::Tab { action, .. } => {
                let change = tab_action(&mut host, action)?;
                let event = if change.opened {
                    Event::PaneOpened {
                        id,
                        pane: change.pane.0,
                        command: host.default_command.clone(),
                    }
                } else {
                    Event::PaneFocused {
                        id,
                        pane: change.pane.0,
                    }
                };
                Ok((CommandResult::Unit, vec![event]))
            }
            Request::Popup {
                rows,
                cols,
                argv,
                env,
                ..
            } => {
                if lock(&host.shared).state.popups().len() >= host.resources.max_popups {
                    anyhow::bail!("configured popup limit reached");
                }
                let original = if argv.is_empty() {
                    host.default_command.clone()
                } else {
                    argv.clone()
                };
                let command = contextual_command(argv, None, env, &host.default_command);
                let pane = host.add_pane(command.clone())?;
                host.set_spawn_metadata(pane, original, None);
                detach_from_active_layout(&mut host, pane)?;
                let replace = {
                    let mut shared = lock(&host.shared);
                    let mut popups = shared.state.popups().to_vec();
                    let z_index = popups
                        .iter()
                        .map(|popup| popup.z_index)
                        .max()
                        .unwrap_or(0)
                        .saturating_add(1);
                    popups.push(crate::state::Popup {
                        pane,
                        width: cols.unwrap_or(80),
                        height: rows.unwrap_or(24),
                        z_index,
                    });
                    let mut candidate = shared.state.clone();
                    if candidate.replace_popups(popups).is_err() {
                        Err(())
                    } else {
                        commit_resource_candidate(&mut shared, candidate)
                    }
                };
                if replace.is_err() {
                    host.discard_unreferenced_pane(pane);
                    anyhow::bail!("popup limit reached");
                }
                Ok((
                    CommandResult::Pane { pane: pane.0 },
                    vec![Event::PaneOpened {
                        id,
                        pane: pane.0,
                        command,
                    }],
                ))
            }
            Request::List { .. } | Request::Workspace { .. } | Request::Subscribe { .. } => {
                anyhow::bail!("operation is not supported by the workspace host")
            }
        })();
        match result {
            Ok((result, events)) => {
                for event in &events {
                    if matches!(event, Event::PaneOpened { .. }) {
                        host.publish_local_event(event.clone());
                    }
                }
                host.start_pending();
                host.apply_geometry();
                if let Some(changed) = &host.changed {
                    changed.pulse();
                }
                (Reply::Completed { id, result }, events)
            }
            Err(error) => (
                crate::control::error_reply(&crate::control::ControlError {
                    id: Some(id),
                    code: ErrorCode::InvalidRequest,
                    message: error.to_string(),
                }),
                Vec::new(),
            ),
        }
    }
}

fn binding_byte(value: &str) -> Option<u8> {
    if let Some(value) = value.strip_prefix("C-")
        && value.len() == 1
    {
        return value
            .bytes()
            .next()
            .map(|byte| byte.to_ascii_uppercase() & 0x1f);
    }
    (value.len() == 1)
        .then(|| value.as_bytes().first().copied())
        .flatten()
}

fn contextual_command(
    argv: Vec<String>,
    cwd: Option<&Path>,
    env: BTreeMap<String, String>,
    default_command: &[String],
) -> Vec<String> {
    let command = if argv.is_empty() {
        default_command.to_vec()
    } else {
        argv
    };
    if cwd.is_none() && env.is_empty() {
        return command;
    }
    let mut wrapped = vec![platform_tool("env")];
    wrapped.extend(env.into_iter().map(|(key, value)| format!("{key}={value}")));
    if let Some(cwd) = cwd {
        // `env` has no portable chdir option, so use positional shell arguments; no
        // user-controlled text is interpolated into shell source.
        let mut shell = vec![
            platform_tool("sh"),
            "-c".to_owned(),
            "cd -- \"$1\" && shift && exec \"$@\"".to_owned(),
            "fux".to_owned(),
            cwd.to_string_lossy().into_owned(),
        ];
        shell.extend(wrapped);
        shell.extend(command);
        shell
    } else {
        wrapped.extend(command);
        wrapped
    }
}

fn platform_tool(name: &str) -> String {
    platform_tool_from(
        name,
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("PREFIX").as_deref(),
        cfg!(target_os = "android"),
    )
}

pub fn platform_tool_from(
    name: &str,
    path_environment: Option<&std::ffi::OsStr>,
    prefix: Option<&std::ffi::OsStr>,
    android: bool,
) -> String {
    if let Some(path) = resolve_zor_path(Path::new(name), path_environment) {
        return path.to_string_lossy().into_owned();
    }
    if let Some(prefix) = prefix {
        let candidate = PathBuf::from(prefix).join("bin").join(name);
        if executable(&candidate) {
            return candidate.to_string_lossy().into_owned();
        }
    }
    if android {
        format!("/system/bin/{name}")
    } else {
        format!("/bin/{name}")
    }
}

fn builtin_command(value: crate::config::BuiltinAction) -> Command {
    use crate::config::BuiltinAction as B;
    match value {
        B::SplitHorizontal => Command::SplitHorizontal,
        B::SplitVertical => Command::SplitVertical,
        B::FocusLeft => Command::Focus(crate::state::Direction::Left),
        B::FocusRight => Command::Focus(crate::state::Direction::Right),
        B::FocusUp => Command::Focus(crate::state::Direction::Up),
        B::FocusDown => Command::Focus(crate::state::Direction::Down),
        B::ClosePane => Command::Close,
        B::NewPane => Command::NewPane,
        B::NewTab => Command::NewTab,
        B::NextTab => Command::NextTab,
        B::PreviousTab => Command::PreviousTab,
        B::Zoom => Command::Zoom,
        B::CopyMode => Command::CopyMode,
        B::Detach => Command::Detach,
        B::WorkspacePicker => Command::WorkspacePicker,
        B::Help => Command::Help,
    }
}

#[derive(Clone, Copy)]
struct TabMutation {
    pane: PaneId,
    opened: bool,
}

fn tab_action(
    host: &mut WorkspaceHost,
    action: crate::control::TabAction,
) -> anyhow::Result<TabMutation> {
    let mut shared = lock(&host.shared);
    let mut tabs = shared.state.tabs().to_vec();
    let active = shared.state.active_tab();
    match action {
        crate::control::TabAction::Next
        | crate::control::TabAction::Previous
        | crate::control::TabAction::Select { .. } => {
            if tabs.is_empty() {
                anyhow::bail!("no tabs");
            }
            let current = active
                .and_then(|id| tabs.iter().position(|tab| tab.id == id))
                .unwrap_or(0);
            let index = match action {
                crate::control::TabAction::Next => (current + 1) % tabs.len(),
                crate::control::TabAction::Previous => {
                    current.checked_sub(1).unwrap_or(tabs.len() - 1)
                }
                crate::control::TabAction::Select { index } => usize::try_from(index)
                    .ok()
                    .filter(|index| *index < tabs.len())
                    .ok_or_else(|| anyhow::anyhow!("tab not found"))?,
                crate::control::TabAction::New { .. } => {
                    return Err(anyhow::anyhow!("invalid tab action"));
                }
            };
            let selected = tabs
                .get(index)
                .map(|tab| tab.id)
                .ok_or_else(|| anyhow::anyhow!("tab not found"))?;
            let mut candidate = shared.state.clone();
            candidate
                .replace_tabs(tabs, Some(selected))
                .map_err(|_| anyhow::anyhow!("invalid tab"))?;
            commit_resource_candidate(&mut shared, candidate)
                .map_err(|_| anyhow::anyhow!("configured workspace resource limit reached"))?;
            let pane = shared
                .state
                .tabs()
                .iter()
                .find(|tab| tab.id == selected)
                .map(|tab| tab.focused)
                .ok_or_else(|| anyhow::anyhow!("tab not found"))?;
            if let Some(changed) = &host.changed {
                changed.pulse();
            }
            Ok(TabMutation {
                pane,
                opened: false,
            })
        }
        crate::control::TabAction::New { name } => {
            if tabs.len() >= host.resources.max_tabs {
                anyhow::bail!("configured tab limit reached");
            }
            drop(shared);
            let pane = host.add_pane(host.default_command.clone())?;
            detach_from_active_layout(host, pane)?;
            let mut shared = lock(&host.shared);
            tabs = shared.state.tabs().to_vec();
            let id = TabId(
                tabs.iter()
                    .map(|tab| tab.id.0)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1),
            );
            tabs.push(Tab {
                id,
                name: name.unwrap_or_else(|| format!("tab-{}", id.0)),
                layout: LayoutTree::new(pane),
                focused: pane,
                zoomed: None,
            });
            let mut candidate = shared.state.clone();
            let failed = candidate.replace_tabs(tabs, Some(id)).is_err()
                || commit_resource_candidate(&mut shared, candidate).is_err();
            if failed {
                drop(shared);
                host.discard_unreferenced_pane(pane);
                anyhow::bail!("tab limit reached");
            }
            if let Some(changed) = &host.changed {
                changed.pulse();
            }
            Ok(TabMutation { pane, opened: true })
        }
    }
}

fn detach_from_active_layout(host: &mut WorkspaceHost, pane: PaneId) -> anyhow::Result<()> {
    let mut shared = lock(&host.shared);
    let active = shared
        .state
        .active_tab()
        .ok_or_else(|| anyhow::anyhow!("no active tab"))?;
    let mut tabs = shared.state.tabs().to_vec();
    let tab = tabs
        .iter_mut()
        .find(|tab| tab.id == active)
        .ok_or_else(|| anyhow::anyhow!("no active tab"))?;
    let next = tab
        .layout
        .close(pane)
        .map_err(|_| anyhow::anyhow!("pane is not in active layout"))?
        .ok_or_else(|| anyhow::anyhow!("cannot detach only pane"))?;
    tab.focused = next;
    let mut candidate = shared.state.clone();
    candidate
        .replace_tabs(tabs, Some(active))
        .map_err(|_| anyhow::anyhow!("invalid layout"))?;
    commit_resource_candidate(&mut shared, candidate)
        .map_err(|_| anyhow::anyhow!("configured workspace resource limit reached"))?;
    Ok(())
}

fn focus_pane(host: &mut WorkspaceHost, pane: PaneId) -> Option<PaneId> {
    let mut shared = lock(&host.shared);
    if !shared.panes.contains_key(&pane) {
        return None;
    }
    let active = shared.state.active_tab()?;
    let mut tabs = shared.state.tabs().to_vec();
    let tab = tabs.iter_mut().find(|tab| tab.id == active)?;
    if !tab.layout.leaves().contains(&pane) {
        return None;
    }
    tab.focused = pane;
    shared.state.replace_tabs(tabs, Some(active)).ok()?;
    Some(pane)
}

fn focus_direction(host: &mut WorkspaceHost, direction: crate::state::Direction) -> Option<PaneId> {
    host.focus(direction);
    let shared = lock(&host.shared);
    let active = shared.state.active_tab()?;
    shared
        .state
        .tabs()
        .iter()
        .find(|tab| tab.id == active)
        .map(|tab| tab.focused)
}

fn write_pane(host: &WorkspaceHost, pane: PaneId, bytes: &[u8]) -> anyhow::Result<()> {
    let runtime = lock(&host.shared)
        .panes
        .get(&pane)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("pane not found"))?;
    let runtime = lock(&runtime);
    let pty = runtime
        .pty
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("pane exited"))?;
    for chunk in bytes.chunks(4096) {
        pty.write_input(chunk)?;
    }
    Ok(())
}

fn write_shared_focused(shared: &Arc<Mutex<Shared>>, bytes: &[u8]) {
    let runtime = {
        let shared = lock(shared);
        focused_pane_id(&shared).and_then(|pane| shared.panes.get(&pane).cloned())
    };
    if let Some(runtime) = runtime
        && let Some(pty) = lock(&runtime).pty.as_ref()
    {
        for chunk in bytes.chunks(4096) {
            if pty.write_input(chunk).is_err() {
                break;
            }
        }
    }
}

fn focused_pane_id(shared: &Shared) -> Option<PaneId> {
    shared
        .state
        .popups()
        .iter()
        .max_by_key(|popup| popup.z_index)
        .map(|popup| popup.pane)
        .or_else(|| {
            shared.state.active_tab().and_then(|active| {
                shared
                    .state
                    .tabs()
                    .iter()
                    .find(|tab| tab.id == active)
                    .map(|tab| tab.focused)
            })
        })
}

fn encode_mouse(
    mouse: MouseEvent,
    column: u16,
    row: u16,
    modes: crate::state::PaneModes,
) -> Option<Vec<u8>> {
    let motion = mouse.code & 32 != 0;
    let button_down = mouse.code & 3 != 3;
    let report = match modes.mouse_mode {
        MouseMode::None => false,
        MouseMode::Press => !mouse.release && !motion,
        MouseMode::PressRelease => !motion,
        MouseMode::ButtonMotion => !motion || button_down,
        MouseMode::AnyMotion => true,
    };
    if !report {
        return None;
    }
    if modes.mouse_encoding == crate::state::MouseEncoding::Sgr {
        return Some(mouse.translated(column, row));
    }
    let code = if mouse.release { 3 } else { mouse.code };
    let values = [
        u32::from(code) + 32,
        u32::from(column) + 32,
        u32::from(row) + 32,
    ];
    let mut bytes = b"\x1b[M".to_vec();
    match modes.mouse_encoding {
        crate::state::MouseEncoding::Default => {
            for value in values {
                bytes.push(u8::try_from(value).ok()?);
            }
        }
        crate::state::MouseEncoding::Utf8 => {
            for value in values {
                let character = char::from_u32(value)?;
                let mut encoded = [0_u8; 4];
                bytes.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
            }
        }
        crate::state::MouseEncoding::Sgr => return None,
    }
    Some(bytes)
}

impl SessionHost for WorkspaceSession {
    type State = WorkspaceState;
    fn snapshot(&mut self) -> Self::State {
        lock(&self.inner).snapshot()
    }
    fn input(&mut self, bytes: &[u8]) {
        lock(&self.inner).input(bytes);
    }
    fn resize(&mut self, client: ClientId, rows: u16, cols: u16) {
        lock(&self.inner).resize(client, rows, cols);
    }
    fn stamp_echo_ack(state: &mut Self::State, echo_ack: u64) {
        WorkspaceHost::stamp_echo_ack(state, echo_ack);
    }
    fn application_cursor(&self) -> bool {
        lock(&self.inner).application_cursor()
    }
    fn alive(&self) -> bool {
        lock(&self.inner).alive()
    }
    fn attach_notify(&mut self, changed: ChangeSignal) {
        lock(&self.inner).attach_notify(changed);
    }
    fn client_detached(&mut self, client: ClientId) {
        lock(&self.inner).client_detached(client);
    }
    fn kill(&mut self) {
        lock(&self.inner).kill();
    }
    fn shutdown(self) {
        lock(&self.inner).shutdown_all();
    }
}

impl SessionHost for WorkspaceHost {
    type State = WorkspaceState;
    fn snapshot(&mut self) -> Self::State {
        self.flush_router_timeout();
        self.reap_exited_panes();
        let mut shared = lock(&self.shared);
        let state = shared.state.clone();
        shared.final_snapshot_pending = false;
        state
    }
    fn input(&mut self, bytes: &[u8]) {
        self.route_input(bytes);
    }
    fn resize(&mut self, client: ClientId, rows: u16, cols: u16) {
        let attached = !self.viewports.contains_key(&client);
        self.resize_order = self.resize_order.saturating_add(1);
        self.viewports
            .insert(client, (rows, cols, self.resize_order));
        self.apply_geometry();
        if let Some(sink) = &lock(&self.shared).event_sink {
            if attached {
                sink.publish(crate::control::Event::ClientAttached {
                    id: 0,
                    client: crate::control::ClientIdentity::Viewer(client.get()),
                });
            }
            sink.publish(crate::control::Event::WorkspaceResized { id: 0, rows, cols });
        }
    }
    fn stamp_echo_ack(state: &mut Self::State, echo_ack: u64) {
        let _ = state.update_metadata(|metadata| {
            metadata.echo_ack = echo_ack;
        });
    }
    fn application_cursor(&self) -> bool {
        self.focused_runtime()
            .is_some_and(|runtime| lock(&runtime).terminal.application_cursor())
    }
    fn alive(&self) -> bool {
        if self.killed {
            return false;
        }
        let shared = lock(&self.shared);
        shared.final_snapshot_pending || shared.panes.values().any(|pane| lock(pane).alive)
    }
    fn attach_notify(&mut self, changed: ChangeSignal) {
        self.changed = Some(changed.clone());
        lock(&self.shared).changed = Some(changed);
        self.start_router_timer();
        self.start_pending();
    }
    fn client_detached(&mut self, client: ClientId) {
        self.viewports.remove(&client);
        self.apply_geometry();
        if let Some(sink) = &lock(&self.shared).event_sink {
            sink.publish(crate::control::Event::ClientDetached {
                id: 0,
                client: crate::control::ClientIdentity::Viewer(client.get()),
            });
        }
    }
    fn kill(&mut self) {
        self.killed = true;
        let (timer, wake) = &*self.router_timer;
        lock(timer).shutdown = true;
        wake.notify_one();
        let processes = lock(&self.external_processes);
        for (pgid, process) in processes.iter() {
            let child = lock(process);
            if child.is_some() {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(-*pgid),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }
        drop(processes);
        let panes: Vec<_> = lock(&self.shared).panes.values().cloned().collect();
        for pane in &panes {
            let mut pane = lock(pane);
            if let Some(pty) = pane.pty.as_mut() {
                let _ = pty.terminate_process_group(false);
            }
        }
        for pane in panes {
            let mut pane = lock(&pane);
            if let Some(pty) = pane.pty.as_mut() {
                let _ = pty.terminate_process_group(true);
            }
        }
    }
    fn shutdown(mut self) {
        self.shutdown_all();
    }
}

impl WorkspaceHost {
    fn shutdown_all(&mut self) {
        self.pending.clear();
        self.kill();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        for worker in self.external_workers.drain(..) {
            let _ = worker.join();
        }
        let panes: Vec<_> = lock(&self.shared).panes.values().cloned().collect();
        for pane in panes {
            if let Some(pty) = lock(&pane).pty.take() {
                pty.shutdown();
            }
        }
    }
}

fn process_chunk(shared: &Arc<Mutex<Shared>>, pane_id: PaneId, bytes: &[u8]) {
    let runtime = { lock(shared).panes.get(&pane_id).cloned() };
    let Some(runtime) = runtime else {
        return;
    };
    let viewport_offset = lock(shared)
        .state
        .pane(pane_id)
        .map_or(0, |pane| pane.viewport_offset);
    let (view, reports, title, clipboard, bell_delta, publish_output) = {
        let mut pane = lock(&runtime);
        let publish_output = output_event_due(&mut pane.last_output_event_ms, monotonic_ms());
        pane.terminal.process(bytes);
        let replies = pane.terminal.take_host_replies();
        if !replies.is_empty()
            && let Some(pty) = pane.pty.as_ref()
        {
            let _ = pty.write_input(&replies);
        }
        let reports: Vec<_> = pane
            .terminal
            .take_unhandled_oscs()
            .into_iter()
            .filter_map(|payload| zor::osc::parse(&payload).ok())
            .collect();
        let exit_code = pane
            .pty
            .as_mut()
            .and_then(|pty| pty.try_wait().ok().flatten())
            .map(|status| status.exit_code());
        if let Some(code) = exit_code {
            pane.alive = false;
            pane.exit_code = Some(code);
            pane.terminal.set_exit_code(code);
        }
        let snapshot = pane.terminal.snapshot();
        let bell_delta = snapshot.bell_count().saturating_sub(pane.last_bell_count);
        let clipboard =
            (snapshot.clipboard() != pane.last_clipboard).then(|| snapshot.clipboard().to_owned());
        let mut visible_screen = snapshot.screen().clone();
        visible_screen.set_scrollback(
            usize::try_from(viewport_offset)
                .unwrap_or(usize::MAX)
                .min(pane.history_limit),
        );
        let view = PaneView::from_vt100(
            &visible_screen,
            snapshot.title().to_owned(),
            AgentStatus::default(),
            viewport_offset,
        )
        .ok();
        (
            view,
            reports,
            snapshot.title().to_owned(),
            clipboard,
            bell_delta,
            publish_output,
        )
    };
    let mut shared = lock(shared);
    if !shared
        .panes
        .get(&pane_id)
        .is_some_and(|current| Arc::ptr_eq(current, &runtime))
    {
        return;
    }
    let old_title = shared
        .state
        .pane(pane_id)
        .map(|pane| pane.title.clone())
        .unwrap_or_default();
    if let Some(mut view) = view {
        if let Some(current) = shared.state.pane(pane_id) {
            view.agent = current.agent.clone();
            view.copy = current.copy;
            view.exit_status = current.exit_status;
        }
        let mut candidate = shared.state.clone();
        if candidate.update_pane(pane_id, |pane| *pane = view).is_ok()
            && state_within_resources(&candidate, &shared.resources)
            && total_resource_units(&shared, &candidate) <= shared.resources.max_units
        {
            shared.state = candidate;
        }
    }
    let mut agent_transitions = Vec::new();
    if let Some(runtime) = shared.panes.get(&pane_id).cloned() {
        for report in reports {
            let changed_report = lock(&runtime).last_report.as_ref() != Some(&report);
            if changed_report {
                let status = AgentStatus::from(&report);
                let previous = shared
                    .state
                    .pane(pane_id)
                    .map(|pane| pane.agent.clone())
                    .unwrap_or_default();
                let mut candidate = shared.state.clone();
                let accepted = candidate
                    .update_pane(pane_id, |pane| pane.agent = status.clone())
                    .is_ok()
                    && total_resource_units(&shared, &candidate) <= shared.resources.max_units;
                if accepted {
                    shared.state = candidate;
                    lock(&runtime).last_report = Some(report);
                }
                if accepted && status != previous {
                    agent_transitions.push((previous, status));
                }
            }
        }
    }
    let selected = shared
        .state
        .active_tab()
        .and_then(|active| shared.state.tabs().iter().find(|tab| tab.id == active))
        .is_some_and(|tab| tab.focused == pane_id);
    let mut candidate = shared.state.clone();
    let metadata_valid = candidate.update_metadata(|metadata| {
        if selected {
            metadata.window_title = title.clone();
        }
        if let Some(clipboard) = &clipboard {
            metadata.clipboard_base64.clone_from(clipboard);
        }
        metadata.bell_count = metadata.bell_count.saturating_add(bell_delta);
        metadata.generation = metadata.generation.saturating_add(1);
    });
    let metadata_accepted = metadata_valid.is_ok()
        && total_resource_units(&shared, &candidate) <= shared.resources.max_units;
    if metadata_accepted {
        shared.state = candidate;
        let mut runtime = lock(&runtime);
        runtime.last_bell_count = runtime.last_bell_count.saturating_add(bell_delta);
        if let Some(clipboard) = clipboard {
            runtime.last_clipboard = clipboard;
        }
    }
    if let Some(sink) = &shared.event_sink {
        if publish_output {
            sink.publish(crate::control::Event::PaneOutput {
                id: 0,
                pane: pane_id.0,
            });
        }
        if metadata_accepted && old_title != title {
            sink.publish(crate::control::Event::PaneTitle {
                id: 0,
                pane: pane_id.0,
                title,
            });
        }
        for (previous, new_agent) in agent_transitions {
            sink.publish(crate::control::Event::AgentState {
                id: 0,
                pane: pane_id.0,
                agent: new_agent.id,
                old_state: control_agent_state(previous.state),
                new_state: control_agent_state(new_agent.state),
                timestamp_ms: monotonic_ms(),
            });
        }
    }
    let changed = shared.changed.clone();
    drop(shared);
    if let Some(changed) = changed {
        changed.pulse();
    }
}

fn state_within_resources(state: &WorkspaceState, limits: &crate::config::ResourceLimits) -> bool {
    state.panes().len() <= limits.max_panes
        && state.tabs().len() <= limits.max_tabs
        && state.popups().len() <= limits.max_popups
        && state.metadata().status.len() <= limits.max_status_segments
        && state
            .panes()
            .values()
            .map(|pane| pane.cells.len())
            .sum::<usize>()
            <= limits.max_total_cells
        && state.recompute_resource_units() <= limits.max_units
}

fn total_resource_units(shared: &Shared, state: &WorkspaceState) -> usize {
    state
        .recompute_resource_units()
        .saturating_add(shared.resource_reservation)
        .saturating_add(
            shared
                .panes
                .values()
                .map(|pane| lock(pane).history_reserved_units)
                .sum::<usize>(),
        )
}

fn commit_resource_candidate(shared: &mut Shared, candidate: WorkspaceState) -> Result<(), ()> {
    if state_within_resources(&candidate, &shared.resources)
        && total_resource_units(shared, &candidate) <= shared.resources.max_units
    {
        shared.state = candidate;
        Ok(())
    } else {
        Err(())
    }
}

fn output_event_due(last: &mut Option<u64>, now: u64) -> bool {
    if last.is_none_or(|previous| now.saturating_sub(previous) >= 250) {
        *last = Some(now);
        true
    } else {
        false
    }
}

fn finish_pane(shared: &Arc<Mutex<Shared>>, pane_id: PaneId) {
    // Serialize natural completion with control removal. Once a runtime is detached, its reader
    // cannot publish a second close event; while attached, `alive` is the single publication bit.
    let runtime = lock(shared).panes.get(&pane_id).cloned();
    let Some(runtime) = runtime else {
        return;
    };
    // EOF on the PTY can become visible just before the child status is waitable. Preserve the
    // terminal status rather than publishing a terminal snapshot with an indeterminate exit.
    let wait_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        let ready = {
            let mut runtime = lock(&runtime);
            runtime.exit_code.is_some()
                || runtime
                    .pty
                    .as_mut()
                    .and_then(|pty| pty.try_wait().ok().flatten())
                    .map(|status| {
                        runtime.exit_code = Some(status.exit_code());
                    })
                    .is_some()
        };
        if ready || std::time::Instant::now() >= wait_deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let mut shared = lock(shared);
    if !shared
        .panes
        .get(&pane_id)
        .is_some_and(|current| Arc::ptr_eq(current, &runtime))
    {
        return;
    }
    let exit = {
        let mut runtime = lock(&runtime);
        if runtime.close_event_published {
            return;
        }
        let code = runtime.exit_code.or_else(|| {
            runtime
                .pty
                .as_mut()
                .and_then(|pty| pty.try_wait().ok().flatten())
                .map(|status| status.exit_code())
        });
        runtime.alive = false;
        runtime.exit_code = code;
        runtime.close_event_published = true;
        code
    };
    let _ = shared
        .state
        .update_pane(pane_id, |pane| pane.exit_status = exit);
    let all_dead = shared.panes.values().all(|pane| !lock(pane).alive);
    remove_exited_pane(&mut shared, pane_id, all_dead);
    if all_dead {
        let _ = shared
            .state
            .update_metadata(|metadata| metadata.exit_code = exit);
        shared.final_snapshot_pending = true;
    }
    if let Some(sink) = &shared.event_sink {
        sink.publish(crate::control::Event::PaneClosed {
            id: 0,
            pane: pane_id.0,
            exit_status: exit.map(|code| i32::try_from(code).unwrap_or(i32::MAX)),
        });
    }
    let changed = shared.changed.clone();
    drop(shared);
    if let Some(changed) = changed {
        changed.pulse();
    }
}

fn remove_exited_pane(shared: &mut Shared, pane_id: PaneId, retain_view: bool) {
    if retain_view {
        shared.panes.remove(&pane_id);
        return;
    }
    let active = shared.state.active_tab();
    let mut tabs = shared.state.tabs().to_vec();
    for tab in &mut tabs {
        if tab.layout.leaves().contains(&pane_id)
            && let Ok(Some(next)) = tab.layout.close(pane_id)
        {
            tab.focused = next;
            tab.zoomed = tab.zoomed.filter(|pane| *pane != pane_id);
        }
    }
    tabs.retain(|tab| !tab.layout.leaves().is_empty() && tab.focused != pane_id);
    let next_active = active
        .filter(|active| tabs.iter().any(|tab| tab.id == *active))
        .or_else(|| tabs.first().map(|tab| tab.id));
    let _ = shared.state.replace_tabs(tabs, next_active);
    let popups = shared
        .state
        .popups()
        .iter()
        .filter(|popup| popup.pane != pane_id)
        .cloned()
        .collect();
    let _ = shared.state.replace_popups(popups);
    if !retain_view {
        let _ = shared.state.remove_pane(pane_id);
    }
    shared.panes.remove(&pane_id);
}

fn control_agent_state(state: AgentState) -> crate::control::AgentStatus {
    match state {
        AgentState::Working => crate::control::AgentStatus::Working,
        AgentState::Blocked => crate::control::AgentStatus::Blocked,
        AgentState::Idle => crate::control::AgentStatus::Idle,
        AgentState::None => crate::control::AgentStatus::None,
    }
}

fn wrapped_command(command: &[String], zor: Option<&Path>) -> Vec<String> {
    if let Some(zor) = zor {
        let mut output = vec![
            zor.to_string_lossy().into_owned(),
            "--title".into(),
            "never".into(),
            "--".into(),
        ];
        output.extend_from_slice(command);
        output
    } else {
        command.to_vec()
    }
}

pub fn resolve_zor_path(
    path: &Path,
    path_environment: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    if path.components().count() > 1 {
        return executable(path).then(|| path.to_owned());
    }
    path_environment
        .into_iter()
        .flat_map(|value| std::env::split_paths(value).collect::<Vec<_>>())
        .map(|directory| directory.join(path))
        .find(|candidate| executable(candidate))
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn monotonic_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    u64::try_from(START.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod host_unit_tests {
    use super::{
        MouseEvent, Shared, WorkspaceHost, commit_resource_candidate, encode_mouse,
        output_event_due, process_chunk, total_resource_units,
    };
    use crate::state::{MouseEncoding, MouseMode, PaneModes};
    use koh::server::{ChangeSignal, SessionHost as _};

    #[test]
    fn pane_output_clock_is_independently_rate_limited() {
        let mut first = None;
        let mut second = None;
        assert!(output_event_due(&mut first, 1_000));
        assert!(!output_event_due(&mut first, 1_249));
        assert!(output_event_due(&mut second, 1_001));
        assert!(output_event_due(&mut first, 1_250));
    }

    #[test]
    fn in_flight_geometry_reservation_blocks_interleaved_state_growth() {
        let mut shared = Shared::default();
        let baseline = total_resource_units(&shared, &shared.state);
        shared.resources.max_units = baseline.saturating_add(32);
        shared.resource_reservation = 32;

        let mut candidate = shared.state.clone();
        candidate
            .update_metadata(|metadata| metadata.window_title.push('x'))
            .expect("bounded title");
        assert!(commit_resource_candidate(&mut shared, candidate).is_err());
        assert_eq!(shared.state.metadata().window_title, "");

        shared.resource_reservation = 0;
        let mut candidate = shared.state.clone();
        candidate
            .update_metadata(|metadata| metadata.window_title.push('x'))
            .expect("bounded title");
        assert!(commit_resource_candidate(&mut shared, candidate).is_ok());
    }

    #[test]
    fn transient_status_and_scrollback_refresh_obey_total_budget() {
        let mut host = WorkspaceHost::spawn(vec!["/bin/cat".into()], 0, None).expect("host");
        let pane = {
            let mut shared = super::lock(&host.shared);
            let pane = *shared.state.panes().keys().next().expect("initial pane");
            let baseline = total_resource_units(&shared, &shared.state);
            shared.resources.max_units = baseline;
            pane
        };

        host.set_transient_status("large", "this allocation must not bypass admission");
        assert!(
            !super::lock(&host.shared)
                .state
                .metadata()
                .status
                .contains_key("large")
        );

        let before = super::lock(&host.shared)
            .state
            .pane(pane)
            .expect("pane")
            .clone();
        let runtime = super::lock(&host.shared)
            .panes
            .get(&pane)
            .expect("runtime")
            .clone();
        super::lock(&runtime)
            .terminal
            .process(b"new scrollback text");
        super::lock(&host.shared).resource_reservation = usize::MAX;
        host.refresh_pane_view(pane);
        assert_eq!(super::lock(&host.shared).state.pane(pane), Some(&before));
        super::lock(&host.shared).resource_reservation = 0;
        host.shutdown_all();
    }

    #[test]
    fn rejected_terminal_reports_are_not_permanently_deduplicated() {
        let mut host = WorkspaceHost::spawn(vec!["/bin/cat".into()], 0, None).expect("host");
        let pane = *super::lock(&host.shared)
            .state
            .panes()
            .keys()
            .next()
            .expect("pane");
        let report = b"\x1b]7877;state=blocked;agent=retry;seq=1;visible=blocker;exited=0\x1b\\";
        super::lock(&host.shared).resource_reservation = usize::MAX;
        process_chunk(&host.shared, pane, report);
        assert_eq!(
            super::lock(&host.shared)
                .state
                .pane(pane)
                .expect("pane")
                .agent
                .state,
            crate::state::AgentState::None
        );
        let runtime = super::lock(&host.shared)
            .panes
            .get(&pane)
            .expect("runtime")
            .clone();
        assert!(super::lock(&runtime).last_report.is_none());

        super::lock(&host.shared).resource_reservation = 0;
        process_chunk(&host.shared, pane, report);
        assert_eq!(
            super::lock(&host.shared)
                .state
                .pane(pane)
                .expect("pane")
                .agent
                .state,
            crate::state::AgentState::Blocked
        );
        host.shutdown_all();
    }

    #[test]
    fn router_uses_one_owned_timer_and_mouse_protocols_are_filtered() {
        let mut host = WorkspaceHost::spawn(vec!["/bin/cat".into()], 32, None).expect("host");
        host.attach_notify(ChangeSignal::default());
        let owned_workers = host.workers.len();
        for _ in 0..100 {
            host.input(b"\x1b[");
        }
        assert_eq!(host.workers.len(), owned_workers);

        let event = MouseEvent {
            code: 32,
            column: 9,
            row: 7,
            release: false,
        };
        let mut modes = PaneModes {
            mouse_mode: MouseMode::PressRelease,
            mouse_encoding: MouseEncoding::Sgr,
            ..PaneModes::default()
        };
        assert!(encode_mouse(event, 9, 7, modes).is_none());
        modes.mouse_mode = MouseMode::AnyMotion;
        assert_eq!(
            encode_mouse(event, 9, 7, modes),
            Some(b"\x1b[<32;9;7M".to_vec())
        );
        modes.mouse_encoding = MouseEncoding::Default;
        assert_eq!(
            encode_mouse(MouseEvent { code: 0, ..event }, 9, 7, modes),
            Some(vec![0x1b, b'[', b'M', 32, 41, 39])
        );
        modes.mouse_encoding = MouseEncoding::Utf8;
        assert!(encode_mouse(MouseEvent { code: 0, ..event }, 300, 7, modes).is_some());
        host.shutdown_all();
    }

    #[test]
    fn mouse_mode_and_encoding_matrix_is_explicit() {
        let modes = |mouse_mode, mouse_encoding| PaneModes {
            mouse_mode,
            mouse_encoding,
            ..PaneModes::default()
        };
        let event = |code, release| MouseEvent {
            code,
            column: 99,
            row: 99,
            release,
        };
        let press = event(0, false);
        let release = event(0, true);
        let drag = event(32, false);
        let hover = event(35, false);
        let wheel = event(64, false);

        for (mode, expected) in [
            (MouseMode::None, [false, false, false, false, false]),
            (MouseMode::Press, [true, false, false, false, true]),
            (MouseMode::PressRelease, [true, true, false, false, true]),
            (MouseMode::ButtonMotion, [true, true, true, false, true]),
            (MouseMode::AnyMotion, [true, true, true, true, true]),
        ] {
            let observed = [press, release, drag, hover, wheel]
                .map(|event| encode_mouse(event, 9, 7, modes(mode, MouseEncoding::Sgr)).is_some());
            assert_eq!(observed, expected, "unexpected filtering for {mode:?}");
        }

        let sgr = modes(MouseMode::AnyMotion, MouseEncoding::Sgr);
        assert_eq!(
            encode_mouse(press, 9, 7, sgr),
            Some(b"\x1b[<0;9;7M".to_vec())
        );
        assert_eq!(
            encode_mouse(release, 9, 7, sgr),
            Some(b"\x1b[<0;9;7m".to_vec())
        );
        assert_eq!(
            encode_mouse(drag, 9, 7, sgr),
            Some(b"\x1b[<32;9;7M".to_vec())
        );
        assert_eq!(
            encode_mouse(wheel, 9, 7, sgr),
            Some(b"\x1b[<64;9;7M".to_vec())
        );

        let default = modes(MouseMode::AnyMotion, MouseEncoding::Default);
        assert_eq!(
            encode_mouse(press, 9, 7, default),
            Some(vec![0x1b, b'[', b'M', 32, 41, 39])
        );
        assert!(encode_mouse(press, 224, 7, default).is_none());

        let utf8 = modes(MouseMode::AnyMotion, MouseEncoding::Utf8);
        assert_eq!(
            encode_mouse(press, 224, 7, utf8),
            Some(b"\x1b[M \xc4\x80'".to_vec())
        );
    }

    #[test]
    fn completed_pane_workers_are_reaped_during_churn() {
        let mut host = WorkspaceHost::spawn(vec!["/bin/cat".into()], 0, None).expect("host");
        host.attach_notify(ChangeSignal::default());
        for _ in 0..16 {
            let _ = host.add_pane(vec!["/usr/bin/true".into()]).expect("pane");
            std::thread::sleep(std::time::Duration::from_millis(15));
            let _ = host.snapshot();
        }
        let _ = host
            .add_pane(vec!["/usr/bin/true".into()])
            .expect("final pane");
        assert!(
            host.workers.len() <= 4,
            "worker handles accumulated: {}",
            host.workers.len()
        );
        host.shutdown_all();
    }
}
