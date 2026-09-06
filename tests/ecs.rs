//! Deterministic ECS tests: injected events and time, no sockets, no processes, no sleeps.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::indexing_slicing
)]

use fux::config::Config;
use fux::ecs::{Effect, Inbound, ManagerAction, ManagerOutcome, Session, ViewerRequest};
use fux::ids::{PaneId, TabId, ViewerId};
use fux::layout::Axis;
use fux::proto::attach::{MouseEvent, ServerMessage};
use fux::proto::control::{
    CommandResult, ErrorCode, Event, FocusTarget, Reply, Request, TabAction, WorkspaceAction,
};
use fux::view::Frame;
use std::collections::BTreeMap;

/// A fake operating system: records effects and completes spawns on demand.
struct Harness {
    session: Session,
    now: u64,
    pending_spawns: Vec<PaneId>,
    next_pid: u32,
    effects: Vec<Effect>,
    frames: BTreeMap<ViewerId, Vec<Frame>>,
    messages: BTreeMap<ViewerId, Vec<ServerMessage>>,
    events: Vec<(String, Event)>,
    released: Vec<PaneId>,
    terminated: Vec<PaneId>,
    written: Vec<(PaneId, Vec<u8>)>,
    manager: Vec<(u64, ManagerOutcome)>,
    control: Vec<(u64, Reply)>,
    opened: Vec<String>,
    closed: Vec<String>,
    idle: bool,
    next_viewer: u64,
}

impl Harness {
    fn new() -> Self {
        let config = Config::from_toml("default-command = { argv = [\"/bin/sh\"] }").unwrap();
        Self {
            session: Session::new(&config).unwrap(),
            now: 1_000,
            pending_spawns: Vec::new(),
            next_pid: 100,
            effects: Vec::new(),
            frames: BTreeMap::new(),
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

    fn step(&mut self, inbound: Vec<Inbound>) -> Vec<Effect> {
        self.now += 10;
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
                        assert!(state.valid());
                        self.frames.entry(*viewer).or_default().push(*state.clone());
                    }
                    self.messages
                        .entry(*viewer)
                        .or_default()
                        .push(message.clone());
                }
                Effect::Event { workspace, event } => {
                    self.events.push((workspace.clone(), event.clone()));
                }
                Effect::ReleasePane { pane } => self.released.push(*pane),
                Effect::Terminate { pane, .. } => self.terminated.push(*pane),
                Effect::WriteInput { pane, bytes } => self.written.push((*pane, bytes.clone())),
                Effect::Manager { token, outcome } => self.manager.push((*token, outcome.clone())),
                Effect::ControlReply { token, reply } => self.control.push((*token, reply.clone())),
                Effect::WorkspaceOpened { name } => self.opened.push(name.clone()),
                Effect::WorkspaceClosed { name } => self.closed.push(name.clone()),
                Effect::Idle => self.idle = true,
                Effect::ResizePty { .. } | Effect::CloseViewer { .. } => {}
            }
        }
        self.effects.extend(effects.iter().cloned());
        effects
    }

    /// Completes every pending spawn successfully and runs the completion step.
    fn complete_spawns(&mut self) -> Vec<Effect> {
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

    fn fail_spawns(&mut self) -> Vec<Effect> {
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

    fn create_workspace(&mut self, name: &str) {
        self.step(vec![Inbound::Manager {
            action: ManagerAction::Resolve {
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

    fn attach(&mut self, workspace: &str, rows: u16, cols: u16) -> ViewerId {
        let viewer = ViewerId(self.next_viewer);
        self.next_viewer += 1;
        self.step(vec![Inbound::ViewerAttached {
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

    fn request(&mut self, viewer: ViewerId, request: ViewerRequest) -> Vec<Effect> {
        self.step(vec![Inbound::ViewerRequest { viewer, request }])
    }

    fn control(&mut self, viewer: ViewerId, request: Request) -> Vec<Effect> {
        self.request(viewer, ViewerRequest::Control(request))
    }

    fn last_frame(&self, viewer: ViewerId) -> &Frame {
        self.frames[&viewer].last().expect("a frame")
    }

    fn replies(&self, viewer: ViewerId) -> Vec<Reply> {
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

    fn pane_ids(&self, viewer: ViewerId) -> Vec<PaneId> {
        self.last_frame(viewer)
            .layout
            .iter()
            .map(|entry| entry.pane)
            .collect()
    }
}

fn split(id: u64, axis: Axis) -> Request {
    Request::Split {
        id,
        axis,
        target: None,
        cwd: None,
        argv: Vec::new(),
    }
}

#[test]
fn fresh_workspace_has_one_tab_and_pane_below_the_bar() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    let frame = harness.last_frame(viewer);
    assert_eq!(frame.workspace, "default");
    assert_eq!(frame.tabs.len(), 1);
    assert_eq!(frame.tabs[0].label, "main");
    assert_eq!(frame.layout.len(), 1);
    assert_eq!(
        frame.layout[0].rect.y, 0,
        "panes start at row 0; the bar is the last row"
    );
    assert_eq!(frame.layout[0].rect.height, 23);
    assert_eq!(frame.focused, Some(PaneId(1)));
    assert_eq!(frame.panes[&PaneId(1)].rows, 23);
    assert_eq!(frame.panes[&PaneId(1)].columns, 80);
    assert!(harness.events.iter().any(|(_, event)| matches!(
        event,
        Event::PaneOpened {
            pane: PaneId(1),
            ..
        }
    )));
    assert!(
        harness
            .events
            .iter()
            .any(|(_, event)| matches!(event, Event::ClientAttached { client: 1, .. }))
    );
}

#[test]
fn split_focus_and_following_input_reach_the_new_pane_only_after_creation() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    // One read: split, then input meant for the new pane.
    harness.step(vec![
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Control(split(1, Axis::Horizontal)),
        },
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Input(b"ls\n".to_vec()),
        },
    ]);
    assert_eq!(harness.pending_spawns, vec![PaneId(2)]);
    assert!(harness.written.is_empty(), "input waited for the barrier");
    assert!(
        harness.replies(viewer).is_empty(),
        "no reply before the spawn completes"
    );
    assert_eq!(harness.pane_ids(viewer), vec![PaneId(1)], "no phantom pane");
    harness.complete_spawns();
    assert_eq!(harness.written, vec![(PaneId(2), b"ls\n".to_vec())]);
    let frame = harness.last_frame(viewer);
    assert_eq!(frame.focused, Some(PaneId(2)));
    assert_eq!(frame.layout.len(), 2);
    let widths: Vec<u16> = frame.layout.iter().map(|entry| entry.rect.width).collect();
    assert_eq!(
        widths,
        vec![39, 40],
        "one cell between siblings carries the separator"
    );
    let replies = harness.replies(viewer);
    assert!(matches!(
        replies.as_slice(),
        [Reply::Completed {
            id: 1,
            result: CommandResult::Pane { pane: PaneId(2) }
        }]
    ));
    // The frame that shows the new pane was published before the reply.
    let messages = &harness.messages[&viewer];
    let state_index = messages
        .iter()
        .rposition(
            |message| matches!(message, ServerMessage::State { state } if state.layout.len() == 2),
        )
        .unwrap();
    let reply_index = messages
        .iter()
        .position(|message| matches!(message, ServerMessage::Reply { .. }))
        .unwrap();
    assert!(state_index < reply_index);
}

#[test]
fn failed_creation_rolls_back_and_releases_the_barrier() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    harness.step(vec![
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Control(split(5, Axis::Vertical)),
        },
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Input(b"x".to_vec()),
        },
    ]);
    harness.fail_spawns();
    assert_eq!(harness.pane_ids(viewer), vec![PaneId(1)]);
    assert_eq!(harness.released, vec![PaneId(2)]);
    assert!(matches!(
        harness.replies(viewer).as_slice(),
        [Reply::Failed { id: 5, .. }]
    ));
    // Input after the failed split reaches the still-focused original pane.
    assert_eq!(harness.written, vec![(PaneId(1), b"x".to_vec())]);
    assert_eq!(harness.session.entity_counts().panes, 1);
    // Ids are never reused.
    harness.control(viewer, split(6, Axis::Vertical));
    assert_eq!(harness.pending_spawns, vec![PaneId(3)]);
}

#[test]
fn stale_targets_fail_without_hitting_replacements() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    harness.control(viewer, split(1, Axis::Horizontal));
    harness.complete_spawns();
    // Confirmed close of pane 2 (captured target), then a replacement pane 3.
    harness.control(
        viewer,
        Request::Kill {
            id: 2,
            pane: PaneId(2),
        },
    );
    assert_eq!(harness.terminated, vec![PaneId(2)]);
    assert_eq!(harness.pane_ids(viewer), vec![PaneId(1)]);
    harness.step(vec![Inbound::PaneExited {
        pane: PaneId(2),
        code: 0,
    }]);
    assert!(harness.released.contains(&PaneId(2)));
    harness.control(viewer, split(3, Axis::Horizontal));
    harness.complete_spawns();
    assert_eq!(harness.pane_ids(viewer), vec![PaneId(1), PaneId(3)]);
    // A late confirmation naming the closed pane fails and pane 3 survives.
    harness.control(
        viewer,
        Request::Kill {
            id: 4,
            pane: PaneId(2),
        },
    );
    let replies = harness.replies(viewer);
    assert!(matches!(
        replies.last(),
        Some(Reply::Failed { id: 4, error }) if error.code == ErrorCode::NotFound
    ));
    assert_eq!(harness.pane_ids(viewer), vec![PaneId(1), PaneId(3)]);
    // Late process reports for a released pane are ignored.
    harness.step(vec![
        Inbound::PaneOutput {
            pane: PaneId(2),
            bytes: b"ghost".to_vec(),
        },
        Inbound::PaneExited {
            pane: PaneId(2),
            code: 9,
        },
    ]);
    assert_eq!(harness.session.entity_counts().panes, 2);
    // Rename by stale tab id fails too.
    harness.control(
        viewer,
        Request::Tab {
            id: 8,
            action: TabAction::Rename {
                tab: TabId(99),
                name: "x".into(),
            },
        },
    );
    assert!(matches!(
        harness.replies(viewer).last(),
        Some(Reply::Failed { id: 8, .. })
    ));
}

#[test]
fn output_eof_and_exit_keep_final_output_and_retire_the_workspace() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    harness.step(vec![
        Inbound::PaneOutput {
            pane: PaneId(1),
            bytes: b"FINAL".to_vec(),
        },
        Inbound::PaneEof { pane: PaneId(1) },
        Inbound::PaneExited {
            pane: PaneId(1),
            code: 29,
        },
    ]);
    let messages = &harness.messages[&viewer];
    let last_frame = messages
        .iter()
        .rev()
        .find_map(|message| match message {
            ServerMessage::State { state } => Some(state),
            _ => None,
        })
        .unwrap();
    let text: String = last_frame.panes[&PaneId(1)]
        .cells
        .iter()
        .map(|cell| cell.text.as_str())
        .collect();
    assert!(
        text.contains("FINAL"),
        "final output painted before retirement"
    );
    assert_eq!(last_frame.exit_code, Some(29));
    assert_eq!(last_frame.panes[&PaneId(1)].exit, Some(29));
    assert!(matches!(
        messages.last(),
        Some(ServerMessage::Exited { code: Some(29) })
    ));
    assert!(harness.events.iter().any(|(_, event)| matches!(
        event,
        Event::PaneClosed {
            pane: PaneId(1),
            exit_status: Some(29),
            ..
        }
    )));
    // The viewer connection closes; retirement completes once the viewer is gone.
    assert!(!harness.closed.contains(&"default".to_owned()));
    harness.step(vec![Inbound::ViewerGone { viewer }]);
    assert_eq!(harness.closed, vec!["default".to_owned()]);
    assert!(harness.released.contains(&PaneId(1)));
    assert!(harness.idle);
    assert_eq!(harness.session.entity_counts(), Default::default());
}

#[test]
fn retirement_grace_expires_without_viewer_acknowledgement() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let _viewer = harness.attach("default", 24, 80);
    harness.step(vec![Inbound::PaneExited {
        pane: PaneId(1),
        code: 3,
    }]);
    assert!(harness.closed.is_empty());
    let deadline = harness
        .session
        .next_deadline_ms()
        .expect("retirement deadline");
    assert!(deadline > harness.now);
    harness.now = deadline;
    harness.step(Vec::new());
    assert_eq!(harness.closed, vec!["default".to_owned()]);
    assert!(harness.idle);
}

#[test]
fn natural_exit_of_one_pane_closes_it_and_of_a_tab_moves_viewers() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    harness.control(viewer, split(1, Axis::Horizontal));
    harness.complete_spawns();
    harness.step(vec![Inbound::PaneExited {
        pane: PaneId(2),
        code: 0,
    }]);
    assert_eq!(harness.pane_ids(viewer), vec![PaneId(1)]);
    assert_eq!(harness.last_frame(viewer).focused, Some(PaneId(1)));
    assert!(harness.released.contains(&PaneId(2)));
    // A second tab whose only pane exits closes the tab; the viewer returns to the first tab.
    harness.control(
        viewer,
        Request::Tab {
            id: 2,
            action: TabAction::New { name: None },
        },
    );
    harness.complete_spawns();
    let frame = harness.last_frame(viewer);
    assert_eq!(frame.tabs.len(), 2);
    assert_eq!(frame.active_tab, Some(TabId(2)));
    assert_eq!(frame.tabs[1].label, "tab-2");
    assert_eq!(frame.layout[0].rect.y, 0, "the bar reserves the last row");
    assert_eq!(frame.layout[0].rect.height, 23);
    harness.step(vec![Inbound::PaneExited {
        pane: PaneId(3),
        code: 0,
    }]);
    let frame = harness.last_frame(viewer);
    assert_eq!(frame.tabs.len(), 1);
    assert_eq!(frame.active_tab, Some(TabId(1)));
    assert_eq!(frame.focused, Some(PaneId(1)));
    assert!(
        harness
            .events
            .iter()
            .any(|(_, event)| matches!(event, Event::TabClosed { tab: TabId(2), .. }))
    );
    assert!(harness.closed.is_empty(), "workspace survives");
}

#[test]
fn viewers_keep_private_tabs_and_focus_while_sharing_layout_edits() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let alice = harness.attach("default", 24, 80);
    let bob = harness.attach("default", 30, 100);
    harness.control(alice, split(1, Axis::Horizontal));
    harness.complete_spawns();
    // Bob sees the split but keeps his focus on pane 1.
    assert_eq!(harness.pane_ids(bob), vec![PaneId(1), PaneId(2)]);
    assert_eq!(harness.last_frame(bob).focused, Some(PaneId(1)));
    assert_eq!(harness.last_frame(alice).focused, Some(PaneId(2)));
    // Geometry is negotiated over the smallest viewer.
    assert_eq!(harness.last_frame(bob).layout[0].rect.height, 23);
    // Alice opens a tab; Bob stays on the first tab.
    harness.control(
        alice,
        Request::Tab {
            id: 2,
            action: TabAction::New {
                name: Some("work".into()),
            },
        },
    );
    harness.complete_spawns();
    assert_eq!(harness.last_frame(alice).active_tab, Some(TabId(2)));
    assert_eq!(harness.last_frame(bob).active_tab, Some(TabId(1)));
    assert_eq!(harness.last_frame(bob).tabs.len(), 2);
    // Bob's input goes to his own focus.
    harness.request(bob, ViewerRequest::Input(b"b".to_vec()));
    assert_eq!(harness.written.last(), Some(&(PaneId(1), b"b".to_vec())));
    harness.request(alice, ViewerRequest::Input(b"a".to_vec()));
    assert_eq!(harness.written.last(), Some(&(PaneId(3), b"a".to_vec())));
    // Directional focus for Bob within tab one.
    harness.control(
        bob,
        Request::Focus {
            id: 9,
            target: FocusTarget::Right,
        },
    );
    assert_eq!(harness.last_frame(bob).focused, Some(PaneId(2)));
    assert_eq!(harness.last_frame(alice).focused, Some(PaneId(3)));
}

#[test]
fn detach_applies_preceding_input_and_drops_the_suffix() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    harness.step(vec![
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Input(b"BEFORE".to_vec()),
        },
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Detach,
        },
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Control(Request::Tab {
                id: 1,
                action: TabAction::New { name: None },
            }),
        },
    ]);
    assert_eq!(harness.written, vec![(PaneId(1), b"BEFORE".to_vec())]);
    assert!(
        harness.pending_spawns.is_empty(),
        "trailing command ignored"
    );
    assert!(matches!(
        harness.messages[&viewer].last(),
        Some(ServerMessage::Exited { code: None })
    ));
    assert_eq!(harness.session.entity_counts().viewers, 0);
    assert_eq!(
        harness.session.entity_counts().panes,
        1,
        "panes survive viewer loss"
    );
    assert!(
        harness
            .events
            .iter()
            .any(|(_, event)| matches!(event, Event::ClientDetached { client: 1, .. }))
    );
}

#[test]
fn workspace_switch_sends_the_suffix_to_the_destination() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    harness.create_workspace("other");
    let viewer = harness.attach("default", 24, 80);
    harness.step(vec![
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Control(Request::Workspace {
                id: 1,
                action: WorkspaceAction::Select {
                    name: "other".into(),
                },
            }),
        },
        Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Input(b"there".to_vec()),
        },
    ]);
    assert_eq!(harness.last_frame(viewer).workspace, "other");
    assert_eq!(harness.written, vec![(PaneId(2), b"there".to_vec())]);
    assert!(harness.events.iter().any(|(name, event)| name == "default" && matches!(event, Event::ClientDetached { .. })));
    assert!(
        harness
            .events
            .iter()
            .any(|(name, event)| name == "other" && matches!(event, Event::ClientAttached { .. }))
    );
    // The no-name attach rule now prefers the most recently attached workspace.
    harness.step(vec![Inbound::Manager {
        action: ManagerAction::Resolve { name: None },
        token: 11,
    }]);
    assert!(matches!(
        harness.manager.last(),
        Some((11, ManagerOutcome::Attach { name, created: false })) if name == "other"
    ));
}

#[test]
fn history_views_are_private_and_clamped() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let alice = harness.attach("default", 8, 40);
    let bob = harness.attach("default", 8, 40);
    let mut output = Vec::new();
    for line in 0..30 {
        output.extend_from_slice(format!("line{line}\r\n").as_bytes());
    }
    harness.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: output,
    }]);
    let before_bob = harness.frames[&bob].len();
    harness.request(
        alice,
        ViewerRequest::View {
            request: 4,
            pane: PaneId(1),
            offset: 1_000,
        },
    );
    let view = harness.messages[&alice]
        .iter()
        .find_map(|message| match message {
            ServerMessage::View { reply } if reply.request == 4 => Some(reply.clone()),
            _ => None,
        })
        .expect("view reply");
    let view_pane = view.view.expect("pane exists");
    assert!(view_pane.offset > 0 && view_pane.offset <= view.history);
    let text: String = view_pane
        .cells
        .iter()
        .map(|cell| cell.text.as_str())
        .collect();
    assert!(text.contains("line0"), "{text}");
    assert_eq!(
        harness.frames[&bob].len(),
        before_bob,
        "reading history did not repaint Bob"
    );
    // A missing pane yields no view.
    harness.request(
        alice,
        ViewerRequest::View {
            request: 5,
            pane: PaneId(77),
            offset: 0,
        },
    );
    assert!(harness.messages[&alice].iter().any(|message| matches!(
        message,
        ServerMessage::View { reply } if reply.request == 5 && reply.view.is_none()
    )));
}

#[test]
fn control_requests_and_mouse_hit_tests_respect_stale_generations() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    harness.step(vec![Inbound::ControlRequest {
        workspace: "default".into(),
        request: Request::List { id: 3 },
        token: 42,
    }]);
    let (token, reply) = harness.control.last().cloned().unwrap();
    assert_eq!(token, 42);
    match reply {
        Reply::Completed {
            result: CommandResult::Listing { workspaces },
            ..
        } => {
            assert_eq!(workspaces[0].name, "default");
            assert_eq!(workspaces[0].viewers, 1);
            assert_eq!(workspaces[0].tabs[0].panes[0].id, PaneId(1));
            assert_eq!(workspaces[0].tabs[0].panes[0].pid, Some(101));
            assert_eq!(workspaces[0].tabs[0].panes[0].geometry.width, 80);
        }
        other => panic!("unexpected reply {other:?}"),
    }
    // Enable SGR mouse in the pane and click inside it with the current generation.
    harness.step(vec![Inbound::PaneOutput {
        pane: PaneId(1),
        bytes: b"\x1b[?1003h\x1b[?1006h".to_vec(),
    }]);
    let generation = harness.last_frame(viewer).generation;
    let click = MouseEvent {
        code: 0,
        column: 5,
        row: 4,
        release: false,
    };
    harness.request(
        viewer,
        ViewerRequest::Mouse {
            event: click,
            generation: generation.wrapping_sub(1),
        },
    );
    assert!(harness.written.is_empty(), "stale generation ignored");
    harness.request(
        viewer,
        ViewerRequest::Mouse {
            event: click,
            generation,
        },
    );
    // Pane-relative coordinates start at the leaf rectangle itself (no frame, panes from row 0):
    // x 4 → column 5, y 3 → row 4.
    assert_eq!(harness.written, vec![(PaneId(1), b"\x1b[<0;5;4M".to_vec())]);
    harness.step(vec![Inbound::ControlRequest {
        workspace: "missing".into(),
        request: Request::List { id: 4 },
        token: 43,
    }]);
    assert!(matches!(
        harness.control.last(),
        Some((43, Reply::Failed { .. }))
    ));
}

#[test]
fn shutdown_terminates_everything_and_reports_idle() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    harness.create_workspace("two");
    let _viewer = harness.attach("default", 24, 80);
    harness.step(vec![Inbound::Shutdown]);
    assert_eq!(harness.terminated.len(), 2);
    harness.step(vec![
        Inbound::PaneExited {
            pane: PaneId(1),
            code: 0,
        },
        Inbound::PaneExited {
            pane: PaneId(2),
            code: 0,
        },
    ]);
    // Viewers were told to exit; once gone, both workspaces close.
    let viewer = ViewerId(1);
    harness.step(vec![Inbound::ViewerGone { viewer }]);
    assert_eq!(harness.closed.len(), 2);
    assert!(harness.idle);
    assert_eq!(harness.session.entity_counts(), Default::default());
}

#[test]
fn limits_and_queue_overflow_are_enforced() {
    let config = Config::from_toml(
        "default-command = { argv = [\"/bin/sh\"] }\n[limits]\nmax-panes = 2\nmax-tabs = 1\n",
    )
    .unwrap();
    let mut harness = Harness::new();
    harness.session = Session::new(&config).unwrap();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    harness.control(viewer, split(1, Axis::Horizontal));
    harness.complete_spawns();
    harness.control(viewer, split(2, Axis::Horizontal));
    assert!(harness.pending_spawns.is_empty());
    assert!(matches!(
        harness.replies(viewer).last(),
        Some(Reply::Failed { id: 2, error }) if error.code == ErrorCode::Limit
    ));
    harness.control(
        viewer,
        Request::Tab {
            id: 3,
            action: TabAction::New { name: None },
        },
    );
    assert!(matches!(
        harness.replies(viewer).last(),
        Some(Reply::Failed { id: 3, error }) if error.code == ErrorCode::Limit
    ));
    // Flooding a blocked viewer disconnects it instead of growing without bound.
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    harness.control(viewer, split(1, Axis::Horizontal));
    let flood: Vec<Inbound> = (0..300)
        .map(|_| Inbound::ViewerRequest {
            viewer,
            request: ViewerRequest::Input(b"x".to_vec()),
        })
        .collect();
    let effects = harness.step(flood);
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::CloseViewer { .. }))
    );
    assert_eq!(harness.session.entity_counts().viewers, 0);
    harness.complete_spawns();
    assert_eq!(
        harness.session.entity_counts().panes,
        2,
        "the pane still joined its tab"
    );
}

#[test]
fn a_viewer_leaving_in_its_arrival_step_is_released_and_the_limit_counts_the_batch() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    // The connection task sends the arrival and the departure back to back when the peer's
    // first read fails; both may land in one step while the spawn is still deferred.
    let ghost = ViewerId(500);
    let effects = harness.step(vec![
        Inbound::ViewerAttached {
            viewer: ghost,
            workspace: "default".into(),
            rows: 10,
            cols: 40,
        },
        Inbound::ViewerGone { viewer: ghost },
    ]);
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::CloseViewer { viewer } if *viewer == ghost))
    );
    assert_eq!(harness.session.entity_counts().viewers, 0);
    let viewer = harness.attach("default", 40, 160);
    let rect = harness.last_frame(viewer).layout[0].rect;
    assert_eq!(
        (rect.height, rect.width),
        (39, 160),
        "no ghost clamps the layout"
    );
    // One step with more arrivals than the limit admits only up to the limit.
    let batch: Vec<Inbound> = (0..66)
        .map(|index| Inbound::ViewerAttached {
            viewer: ViewerId(1_000 + index),
            workspace: "default".into(),
            rows: 24,
            cols: 80,
        })
        .collect();
    let effects = harness.step(batch);
    let refused = effects
        .iter()
        .filter(|effect| {
            matches!(
                effect,
                Effect::ToViewer {
                    message: ServerMessage::Error { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(refused, 3, "one viewer was already attached");
    assert_eq!(harness.session.entity_counts().viewers, 64);
}

// ---------------------------------------------------------------------------------------------
// Randomized command sequences: every reachable interleaving of creations, stale ids, delayed or
// failed completions, viewer churn, process reports and time must keep the World consistent and
// must drain to an idle, empty World after shutdown.
// ---------------------------------------------------------------------------------------------

mod randomized {
    use super::*;
    use proptest::prelude::*;

    const NAMES: [&str; 3] = ["default", "alpha", "beta"];

    #[derive(Clone, Debug)]
    enum Op {
        Attach { workspace: u8, rows: u16, cols: u16 },
        Gone { viewer: u8 },
        Viewer { viewer: u8, request: ViewerRequest },
        Control { workspace: u8, request: Request },
        Manager(ManagerAction),
        Output { pane: u8, bytes: Vec<u8> },
        Eof { pane: u8 },
        Exited { pane: u8, code: u32 },
        Complete { index: u8, ok: bool },
        Wait(u64),
    }

    fn pane() -> impl Strategy<Value = PaneId> {
        (1..=12u32).prop_map(PaneId)
    }

    fn tab() -> impl Strategy<Value = TabId> {
        (1..=8u32).prop_map(TabId)
    }

    fn name() -> impl Strategy<Value = String> {
        (0..3usize).prop_map(|index| NAMES[index].to_owned())
    }

    fn request() -> impl Strategy<Value = Request> {
        prop_oneof![
            any::<bool>().prop_map(|stacked| split(
                1,
                if stacked {
                    Axis::Vertical
                } else {
                    Axis::Horizontal
                }
            )),
            pane().prop_map(|pane| Request::Kill { id: 1, pane }),
            prop_oneof![
                Just(FocusTarget::Left),
                Just(FocusTarget::Right),
                Just(FocusTarget::Up),
                Just(FocusTarget::Down),
                pane().prop_map(FocusTarget::Pane),
            ]
            .prop_map(|target| Request::Focus { id: 1, target }),
            (pane(), prop_oneof![Just(-3i16), Just(2), Just(40)])
                .prop_map(|(pane, delta)| Request::Resize { id: 1, pane, delta }),
            pane().prop_map(|pane| Request::SendKeys {
                id: 1,
                pane,
                keys: "x\\n".into(),
            }),
            pane().prop_map(|pane| Request::Capture {
                id: 1,
                pane,
                attrs: false,
                scrollback: 5,
                max_bytes: 4096,
            }),
            Just(Request::List { id: 1 }),
            prop_oneof![
                Just(TabAction::New { name: None }),
                Just(TabAction::Next),
                Just(TabAction::Previous),
                (0..4u32).prop_map(|index| TabAction::Select { index }),
                tab().prop_map(|tab| TabAction::SelectId { tab }),
                tab().prop_map(|tab| TabAction::Rename {
                    tab,
                    name: "renamed".into(),
                }),
                tab().prop_map(|tab| TabAction::Close { tab }),
            ]
            .prop_map(|action| Request::Tab { id: 1, action }),
            prop_oneof![
                Just(WorkspaceAction::List),
                Just(WorkspaceAction::New { name: None }),
                name().prop_map(|name| WorkspaceAction::New { name: Some(name) }),
                name().prop_map(|name| WorkspaceAction::Kill { name }),
                name().prop_map(|name| WorkspaceAction::Select { name }),
            ]
            .prop_map(|action| Request::Workspace { id: 1, action }),
        ]
    }

    fn viewer_request() -> impl Strategy<Value = ViewerRequest> {
        prop_oneof![
            4 => request().prop_map(ViewerRequest::Control),
            2 => prop::collection::vec(any::<u8>(), 0..8).prop_map(ViewerRequest::Input),
            1 => (0..70u16, 1..=30u16, 1..=90u16, any::<bool>(), 0..6u64).prop_map(
                |(code, row, column, release, generation)| ViewerRequest::Mouse {
                    event: MouseEvent {
                        code,
                        column,
                        row,
                        release,
                    },
                    generation,
                }
            ),
            1 => (pane(), 0..30u32).prop_map(|(pane, offset)| ViewerRequest::View {
                request: 1,
                pane,
                offset,
            }),
            1 => (1..=40u16, 1..=120u16).prop_map(|(rows, cols)| ViewerRequest::Resize { rows, cols }),
            1 => Just(ViewerRequest::Detach),
        ]
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            2 => (0..3u8, 1..=30u16, 1..=100u16).prop_map(|(workspace, rows, cols)| Op::Attach {
                workspace,
                rows,
                cols,
            }),
            1 => (0..6u8).prop_map(|viewer| Op::Gone { viewer }),
            8 => (0..6u8, viewer_request()).prop_map(|(viewer, request)| Op::Viewer { viewer, request }),
            3 => (0..3u8, request()).prop_map(|(workspace, request)| Op::Control { workspace, request }),
            2 => prop_oneof![
                Just(ManagerAction::List),
                Just(ManagerAction::Resolve { name: None }),
                name().prop_map(|name| ManagerAction::Resolve { name: Some(name) }),
                name().prop_map(|name| ManagerAction::Kill { name }),
            ]
            .prop_map(Op::Manager),
            3 => (0..13u8, prop::collection::vec(any::<u8>(), 0..64))
                .prop_map(|(pane, bytes)| Op::Output { pane, bytes }),
            1 => (0..13u8).prop_map(|pane| Op::Eof { pane }),
            2 => (0..13u8, 0..3u32).prop_map(|(pane, code)| Op::Exited { pane, code }),
            5 => (0..4u8, prop::bool::weighted(0.8)).prop_map(|(index, ok)| Op::Complete { index, ok }),
            1 => prop_oneof![Just(100u64), Just(4_000), Just(6_000)].prop_map(Op::Wait),
        ]
    }

    fn drain_after_shutdown(harness: &mut Harness) {
        harness.step(vec![Inbound::Shutdown]);
        for _ in 0..8 {
            let mut inbound = Vec::new();
            for pane in std::mem::take(&mut harness.pending_spawns) {
                inbound.push(Inbound::SpawnCompleted {
                    pane,
                    result: Err("shutdown".into()),
                });
            }
            for pane in std::mem::take(&mut harness.terminated) {
                inbound.push(Inbound::PaneEof { pane });
                inbound.push(Inbound::PaneExited { pane, code: 129 });
            }
            harness.now += 6_000;
            let quiet = inbound.is_empty();
            harness.step(inbound);
            if harness.idle && quiet && harness.terminated.is_empty() {
                break;
            }
        }
    }

    proptest! {
        #[test]
        fn invariants_survive_random_sequences_stale_ids_and_delayed_completions(
            ops in prop::collection::vec(op(), 1..80)
        ) {
            let mut harness = Harness::new();
            harness.create_workspace("default");
            let mut viewers: Vec<ViewerId> = Vec::new();
            for op in ops {
                match op {
                    Op::Attach { workspace, rows, cols } => {
                        let viewer = ViewerId(harness.next_viewer);
                        harness.next_viewer += 1;
                        harness.step(vec![Inbound::ViewerAttached {
                            viewer,
                            workspace: NAMES[usize::from(workspace)].to_owned(),
                            rows,
                            cols,
                        }]);
                        viewers.push(viewer);
                    }
                    Op::Gone { viewer } => {
                        let viewer = ViewerId(u64::from(viewer) + 1);
                        harness.step(vec![Inbound::ViewerGone { viewer }]);
                    }
                    Op::Viewer { viewer, request } => {
                        let viewer = ViewerId(u64::from(viewer) + 1);
                        harness.step(vec![Inbound::ViewerRequest { viewer, request }]);
                    }
                    Op::Control { workspace, request } => {
                        harness.step(vec![Inbound::ControlRequest {
                            workspace: NAMES[usize::from(workspace)].to_owned(),
                            request,
                            token: 9,
                        }]);
                    }
                    Op::Manager(action) => {
                        harness.step(vec![Inbound::Manager { action, token: 8 }]);
                    }
                    Op::Output { pane, bytes } => {
                        harness.step(vec![Inbound::PaneOutput {
                            pane: PaneId(u32::from(pane)),
                            bytes,
                        }]);
                    }
                    Op::Eof { pane } => {
                        harness.step(vec![Inbound::PaneEof {
                            pane: PaneId(u32::from(pane)),
                        }]);
                    }
                    Op::Exited { pane, code } => {
                        harness.step(vec![Inbound::PaneExited {
                            pane: PaneId(u32::from(pane)),
                            code,
                        }]);
                    }
                    Op::Complete { index, ok } => {
                        if harness.pending_spawns.is_empty() {
                            continue;
                        }
                        let index = usize::from(index) % harness.pending_spawns.len();
                        let pane = harness.pending_spawns.remove(index);
                        harness.next_pid += 1;
                        let result = if ok {
                            Ok(harness.next_pid)
                        } else {
                            Err("exec failed".into())
                        };
                        harness.step(vec![Inbound::SpawnCompleted { pane, result }]);
                    }
                    Op::Wait(ms) => {
                        harness.now += ms;
                        harness.step(Vec::new());
                    }
                }
                // Frames never show a pane that is still starting or already gone.
                for frames in harness.frames.values() {
                    for frame in frames {
                        prop_assert!(frame.valid());
                    }
                }
            }
            drain_after_shutdown(&mut harness);
            let counts = harness.session.entity_counts();
            prop_assert_eq!(
                (counts.workspaces, counts.tabs, counts.panes, counts.viewers),
                (0, 0, 0, 0),
                "shutdown must drain every entity"
            );
            prop_assert!(harness.idle, "server reported idle after shutdown");
            prop_assert!(harness.pending_spawns.is_empty());
            let _ = viewers;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Regressions from the independent review.
// ---------------------------------------------------------------------------------------------

#[test]
fn exit_arriving_with_or_before_the_spawn_completion_is_not_lost() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    let viewer = harness.attach("default", 24, 80);
    // Completion and exit in one batch: the reader thread starts with the process.
    harness.control(viewer, split(1, Axis::Horizontal));
    assert_eq!(harness.pending_spawns, vec![PaneId(2)]);
    harness.pending_spawns.clear();
    harness.step(vec![
        Inbound::SpawnCompleted {
            pane: PaneId(2),
            result: Ok(200),
        },
        Inbound::PaneEof { pane: PaneId(2) },
        Inbound::PaneExited {
            pane: PaneId(2),
            code: 3,
        },
    ]);
    assert_eq!(harness.pane_ids(viewer), vec![PaneId(1)], "dead pane left");
    assert_eq!(harness.last_frame(viewer).focused, Some(PaneId(1)));
    assert!(harness.released.contains(&PaneId(2)));
    assert!(
        harness.terminated.is_empty(),
        "no signal for a reaped process"
    );
    assert!(harness.events.iter().any(|(_, event)| matches!(
        event,
        Event::PaneClosed {
            pane: PaneId(2),
            exit_status: Some(3),
            ..
        }
    )));
    // Exit reported one step before the completion.
    harness.control(viewer, split(2, Axis::Vertical));
    assert_eq!(harness.pending_spawns, vec![PaneId(3)]);
    harness.step(vec![Inbound::PaneExited {
        pane: PaneId(3),
        code: 5,
    }]);
    assert_eq!(
        harness.pane_ids(viewer),
        vec![PaneId(1)],
        "reservation stays hidden"
    );
    harness.complete_spawns();
    assert_eq!(harness.pane_ids(viewer), vec![PaneId(1)]);
    assert!(harness.released.contains(&PaneId(3)));
    assert_eq!(harness.session.entity_counts().panes, 1);
    assert!(harness.events.iter().any(|(_, event)| matches!(
        event,
        Event::PaneClosed {
            pane: PaneId(3),
            exit_status: Some(5),
            ..
        }
    )));
}

#[test]
fn a_workspace_whose_first_pane_exits_at_once_retires_with_its_status() {
    let mut harness = Harness::new();
    harness.step(vec![Inbound::Manager {
        action: ManagerAction::Resolve {
            name: Some("default".into()),
        },
        token: 7,
    }]);
    assert_eq!(harness.pending_spawns, vec![PaneId(1)]);
    harness.pending_spawns.clear();
    harness.step(vec![
        Inbound::SpawnCompleted {
            pane: PaneId(1),
            result: Ok(100),
        },
        Inbound::PaneEof { pane: PaneId(1) },
        Inbound::PaneExited {
            pane: PaneId(1),
            code: 7,
        },
    ]);
    assert!(harness.events.iter().any(|(_, event)| matches!(
        event,
        Event::PaneClosed {
            pane: PaneId(1),
            exit_status: Some(7),
            ..
        }
    )));
    // Nobody is watching, so the workspace finalizes at once and the server goes idle.
    assert!(harness.closed.contains(&"default".to_owned()));
    assert!(harness.idle);
    let counts = harness.session.entity_counts();
    assert_eq!((counts.workspaces, counts.panes), (0, 0));
}

#[test]
fn killing_a_workspace_with_a_pending_spawn_stops_the_late_process() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    harness.step(vec![Inbound::ControlRequest {
        workspace: "default".into(),
        request: split(1, Axis::Vertical),
        token: 3,
    }]);
    assert_eq!(harness.pending_spawns, vec![PaneId(2)]);
    harness.step(vec![Inbound::Manager {
        action: ManagerAction::Kill {
            name: "default".into(),
        },
        token: 8,
    }]);
    assert!(harness.closed.contains(&"default".to_owned()));
    assert!(
        harness
            .control
            .iter()
            .any(|(token, reply)| *token == 3 && matches!(reply, Reply::Failed { .. })),
        "the pending creation failed its requester"
    );
    harness.terminated.clear();
    harness.released.clear();
    // The process reports in after its reservation was released: it must be stopped and reaped.
    harness.complete_spawns();
    assert!(harness.terminated.contains(&PaneId(2)));
    assert!(harness.released.contains(&PaneId(2)));
    assert_eq!(harness.session.entity_counts().panes, 0);
}

#[test]
fn viewer_requests_never_reach_other_workspaces() {
    let mut harness = Harness::new();
    harness.create_workspace("default");
    harness.create_workspace("other");
    let viewer = harness.attach("default", 24, 80);
    harness.step(vec![Inbound::PaneOutput {
        pane: PaneId(2),
        bytes: b"SECRET".to_vec(),
    }]);
    harness.request(
        viewer,
        ViewerRequest::View {
            request: 4,
            pane: PaneId(2),
            offset: 0,
        },
    );
    let view = harness.messages[&viewer]
        .iter()
        .rev()
        .find_map(|message| match message {
            ServerMessage::View { reply } => Some(reply.clone()),
            _ => None,
        })
        .expect("view reply");
    assert!(view.view.is_none(), "foreign pane is invisible");
    harness.control(
        viewer,
        Request::Workspace {
            id: 5,
            action: WorkspaceAction::Kill {
                name: "other".into(),
            },
        },
    );
    assert!(matches!(
        harness.replies(viewer).last(),
        Some(Reply::Failed { error, .. }) if error.code == ErrorCode::Unauthorized
    ));
    assert!(!harness.closed.contains(&"other".to_owned()));
    // Listing and switching remain available through the same attachment.
    harness.control(
        viewer,
        Request::Workspace {
            id: 6,
            action: WorkspaceAction::Select {
                name: "other".into(),
            },
        },
    );
    assert_eq!(harness.last_frame(viewer).workspace, "other");
}
