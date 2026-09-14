//! A fake operating system around a `Session`: records effects, completes spawns on demand,
//! and the request helpers the topic modules share.
use super::*;

pub fn input_request(harness: &mut Harness, workspace: &str, request: Request) -> Reply {
    harness.step(vec![Inbound::ControlRequest {
        workspace: workspace.into(),
        request,
        token: 900,
    }]);
    harness.control.last().expect("input reply").1.clone()
}

pub fn final_reply(h: &mut Harness, pane: PaneId, instance: &str) -> Reply {
    h.step(vec![Inbound::Manager {
        request: ManagerRequest::Final {
            instance: instance.into(),
            pane,
        },
        token: 402,
    }]);
    match &h.manager.last().expect("final reply").1 {
        ManagerOutcome::Reply(ManagerReply::Final { result: reply }) => reply.clone(),
        other => panic!("unexpected final reply: {other:?}"),
    }
}

pub fn input_receipt(reply: Reply) -> fux::proto::control::InputReceipt {
    match reply {
        Reply::Completed {
            result: CommandResult::Input { receipt },
            ..
        } => receipt,
        other => panic!("expected input receipt, got {other:?}"),
    }
}

/// A fake operating system: records effects and completes spawns on demand.
pub struct Harness {
    pub session: Session,
    pub now: u64,
    pub pending_spawns: Vec<PaneId>,
    pub next_pid: u32,
    pub effects: Vec<Effect>,
    pub frames: BTreeMap<ViewerId, Vec<Frame>>,
    /// The frame each viewer holds after applying every update it received.
    pub retained: BTreeMap<ViewerId, Frame>,
    pub messages: BTreeMap<ViewerId, Vec<ServerMessage>>,
    pub events: Vec<(String, Event)>,
    pub released: Vec<PaneId>,
    pub terminated: Vec<PaneId>,
    pub written: Vec<(PaneId, Vec<u8>)>,
    pub manager: Vec<(u64, ManagerOutcome)>,
    pub control: Vec<(u64, Reply)>,
    pub opened: Vec<String>,
    pub closed: Vec<String>,
    pub idle: bool,
    pub next_viewer: u64,
}

impl Harness {
    pub fn new() -> Self {
        let config = Config::from_toml("default-command = { argv = [\"/bin/sh\"] }").unwrap();
        Self {
            session: {
                let mut session = Session::new(&config).unwrap();
                session.set_identity(fux::ecs::ServerIdentity {
                    instance_nonce: "test-instance".into(),
                    ..Default::default()
                });
                session
            },
            now: 1_000,
            pending_spawns: Vec::new(),
            next_pid: 100,
            effects: Vec::new(),
            frames: BTreeMap::new(),
            retained: BTreeMap::new(),
            messages: BTreeMap::new(),
            events: Vec::new(),
            released: Vec::new(),
            terminated: Vec::new(),
            written: Vec::new(),
            manager: Vec::new(),
            control: Vec::new(),
            opened: Vec::new(),
            closed: Vec::new(),
            idle: false,
            next_viewer: 1,
        }
    }

    pub fn step(&mut self, inbound: Vec<Inbound>) -> Vec<Effect> {
        self.step_after(10, inbound)
    }

    /// Runs one step `elapsed_ms` after the previous one (`step` uses 10 ms, beyond the frame
    /// interval, so every step there may publish).
    pub fn step_after(&mut self, elapsed_ms: u64, inbound: Vec<Inbound>) -> Vec<Effect> {
        self.now += elapsed_ms;
        let effects = self.session.step(self.now, inbound);
        assert_eq!(self.session.retained_messages(), 0, "messages retained");
        self.session
            .check_invariants()
            .unwrap_or_else(|error| panic!("invariant violated: {error}"));
        for effect in &effects {
            match effect {
                Effect::SpawnPane { pane, .. } => self.pending_spawns.push(*pane),
                Effect::ToViewer { viewer, message } => {
                    if let ServerMessage::State { state } = message {
                        let current = self.retained.entry(*viewer).or_default();
                        current
                            .apply((**state).clone())
                            .unwrap_or_else(|error| panic!("invalid update: {error}"));
                        self.frames
                            .entry(*viewer)
                            .or_default()
                            .push(current.clone());
                    }
                    self.messages
                        .entry(*viewer)
                        .or_default()
                        .push(message.clone());
                }
                Effect::Event {
                    workspace, event, ..
                } => {
                    self.events.push((workspace.clone(), event.clone()));
                }
                Effect::ReleasePane { pane } => self.released.push(*pane),
                Effect::Terminate { pane, .. } => self.terminated.push(*pane),
                Effect::WriteTrackedInput { pane, bytes, .. }
                | Effect::WriteInput { pane, bytes } => self.written.push((*pane, bytes.clone())),
                Effect::Manager { token, outcome } => self.manager.push((*token, outcome.clone())),
                Effect::ControlReply { token, reply } => self.control.push((*token, reply.clone())),
                Effect::WorkspaceOpened { name, .. } => self.opened.push(name.clone()),
                Effect::WorkspaceClosed { name } => self.closed.push(name.clone()),
                Effect::Idle => self.idle = true,
                Effect::ResizePty { .. } | Effect::CloseViewer { .. } => {}
            }
        }
        self.effects.extend(effects.iter().cloned());
        effects
    }

    /// Completes every pending spawn successfully and runs the completion step.
    pub fn complete_spawns(&mut self) -> Vec<Effect> {
        let pending: Vec<PaneId> = std::mem::take(&mut self.pending_spawns);
        let inbound = pending
            .into_iter()
            .map(|pane| {
                self.next_pid += 1;
                Inbound::SpawnCompleted {
                    pane,
                    result: Ok(self.next_pid),
                }
            })
            .collect();
        self.step(inbound)
    }

    pub fn fail_spawns(&mut self) -> Vec<Effect> {
        let pending: Vec<PaneId> = std::mem::take(&mut self.pending_spawns);
        let inbound = pending
            .into_iter()
            .map(|pane| Inbound::SpawnCompleted {
                pane,
                result: Err("exec failed".into()),
            })
            .collect();
        self.step(inbound)
    }

    pub fn create_workspace(&mut self, name: &str) {
        self.step(vec![Inbound::Manager {
            request: ManagerRequest::Resolve {
                name: Some(name.into()),
            },
            token: 7,
        }]);
        assert_eq!(self.pending_spawns.len(), 1, "initial pane spawn requested");
        self.complete_spawns();
        assert!(self.opened.contains(&name.to_owned()));
        assert!(matches!(
            self.manager.last(),
            Some((7, ManagerOutcome::Attach { created: true, .. }))
        ));
        self.manager.clear();
    }

    pub fn attach(&mut self, workspace: &str, rows: u16, cols: u16) -> ViewerId {
        let viewer = ViewerId(self.next_viewer);
        self.next_viewer += 1;
        self.step(vec![Inbound::ViewerAttached {
            initial: None,
            viewer,
            workspace: workspace.into(),
            rows,
            cols,
        }]);
        assert!(
            self.frames
                .get(&viewer)
                .is_some_and(|frames| !frames.is_empty())
        );
        viewer
    }

    pub fn request(&mut self, viewer: ViewerId, request: ViewerRequest) -> Vec<Effect> {
        self.step(vec![Inbound::ViewerRequest { viewer, request }])
    }

    pub fn control(&mut self, viewer: ViewerId, request: Request) -> Vec<Effect> {
        self.request(viewer, ViewerRequest::Control(request))
    }

    pub fn last_frame(&self, viewer: ViewerId) -> &Frame {
        self.frames[&viewer].last().expect("a frame")
    }

    pub fn replies(&self, viewer: ViewerId) -> Vec<Reply> {
        self.messages
            .get(&viewer)
            .into_iter()
            .flatten()
            .filter_map(|message| match message {
                ServerMessage::Reply { reply } => Some(reply.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn pane_ids(&self, viewer: ViewerId) -> Vec<PaneId> {
        self.last_frame(viewer)
            .layout
            .iter()
            .map(|entry| entry.pane)
            .collect()
    }
}

pub fn split(id: u64, axis: Axis) -> Request {
    Request::Split {
        stream: None,
        instance: None,
        id,
        axis,
        target: None,
        cwd: None,
        argv: Vec::new(),
        env: Vec::new(),
        rows: None,
        columns: None,
        final_retain_ms: fux::config::DEFAULT_FINAL_RETAIN_MS,
        fixed_workspace: false,
        right_click: Default::default(),
        ratio: 5000,
        focus: true,
    }
}

pub fn pane_seq(harness: &mut Harness, viewer: ViewerId, pane: PaneId) -> u64 {
    harness.control(
        viewer,
        Request::List {
            instance: None,
            id: 900,
        },
    );
    match harness.replies(viewer).last() {
        Some(Reply::Completed {
            result: CommandResult::Listing { workspaces, .. },
            ..
        }) => workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| &tab.panes)
            .find(|summary| summary.id == pane)
            .map(|summary| summary.seq)
            .expect("pane listed"),
        other => panic!("unexpected listing reply {other:?}"),
    }
}

/// A cells capture of the whole visible grid: the sequence it reflects and its lines.
pub fn cells_capture(
    harness: &mut Harness,
    viewer: ViewerId,
    pane: PaneId,
) -> (u64, Vec<fux::proto::control::CaptureLine>) {
    harness.control(
        viewer,
        Request::Capture {
            if_revision: None,
            instance: None,
            id: 901,
            pane,
            attrs: false,
            scrollback: 0,
            max_bytes: 65536,
            format: fux::proto::control::CaptureFormat::Cells,
        },
    );
    match harness.replies(viewer).last() {
        Some(Reply::Completed {
            result: CommandResult::Cells { seq, lines, .. },
            ..
        }) => (*seq, lines.clone()),
        other => panic!("unexpected capture reply {other:?}"),
    }
}

/// The text a captured line carries, blanks as spaces and trailing blanks trimmed.
pub fn line_text(line: &fux::proto::control::CaptureLine) -> String {
    let mut text = String::new();
    for cell in &line.cells {
        match (&cell.text, cell.kind) {
            (_, Some(fux::view::CellKind::WideContinuation)) => {}
            (Some(t), _) => text.push_str(t),
            (None, _) => text.extend(std::iter::repeat_n(' ', usize::from(cell.run.max(1)))),
        }
    }
    text.trim_end_matches(' ').to_owned()
}

pub fn layout_request(
    generation: Option<u64>,
    action: fux::proto::control::LayoutAction,
) -> Request {
    Request::Layout {
        id: 950,
        instance: Some("test-instance".into()),
        tab: TabId(1),
        generation,
        action,
    }
}

pub fn exported_layout(h: &mut Harness) -> (u64, fux::layout::LayoutDocument<PaneId>) {
    let result = input_request(
        h,
        "default",
        layout_request(None, fux::proto::control::LayoutAction::Export),
    );
    match result {
        Reply::Completed {
            result:
                CommandResult::Layout {
                    generation,
                    document,
                    ..
                },
            ..
        } => (generation, document),
        other => panic!("layout export: {other:?}"),
    }
}

pub fn workspace_stream(h: &mut Harness, name: &str) -> u64 {
    match input_request(
        h,
        name,
        Request::List {
            id: 0,
            instance: Some("test-instance".into()),
        },
    ) {
        Reply::Completed {
            result: CommandResult::Listing { workspaces, .. },
            ..
        } => workspaces[0].event_cursor.stream,
        other => panic!("listing: {other:?}"),
    }
}

pub fn transfer_request(
    h: &mut Harness,
    transfer: fux::proto::control::WorkspaceTransfer,
) -> Reply {
    h.step(vec![Inbound::Manager {
        request: ManagerRequest::Transfer { transfer },
        token: 990,
    }]);
    match &h.manager.last().unwrap().1 {
        ManagerOutcome::Reply(ManagerReply::Layout { result: reply }) => reply.clone(),
        other => panic!("transfer: {other:?}"),
    }
}

pub fn pane_location(h: &mut Harness, instance: &str, pane: PaneId) -> Reply {
    h.step(vec![Inbound::Manager {
        token: 991,
        request: ManagerRequest::PaneLocation {
            instance: instance.into(),
            pane,
        },
    }]);
    let ManagerOutcome::Reply(ManagerReply::PaneLocation { result: reply }) =
        &h.manager.last().unwrap().1
    else {
        panic!("location reply missing")
    };
    reply.clone()
}

pub fn manager_input_status(
    h: &mut Harness,
    instance: &str,
    pane: PaneId,
    operation: u64,
) -> Reply {
    h.step(vec![Inbound::Manager {
        token: 992,
        request: ManagerRequest::InputStatus {
            instance: instance.into(),
            pane,
            operation,
        },
    }]);
    let ManagerOutcome::Reply(ManagerReply::InputStatus { result: reply }) =
        &h.manager.last().unwrap().1
    else {
        panic!("receipt reply missing")
    };
    reply.clone()
}
