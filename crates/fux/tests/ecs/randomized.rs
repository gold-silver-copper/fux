//! Randomized command sequences: every reachable interleaving of creations, stale ids, delayed or
//! failed completions, viewer churn, process reports and time must keep the World consistent and
//! must drain to an idle, empty World after shutdown.

use super::*;
use proptest::prelude::*;

const NAMES: [&str; 3] = ["default", "alpha", "beta"];

#[derive(Clone, Debug)]
enum Op {
    Attach { workspace: u8, rows: u16, cols: u16 },
    Gone { viewer: u8 },
    Viewer { viewer: u8, request: ViewerRequest },
    Control { workspace: u8, request: Request },
    Manager(ManagerRequest),
    Output { pane: u8, bytes: Vec<u8> },
    Eof { pane: u8 },
    Exited { pane: u8, code: u32 },
    Complete { index: u8, ok: bool },
    Advance(u64),
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
        pane().prop_map(|pane| Request::Kill {
            instance: None,
            id: 1,
            pane
        }),
        prop_oneof![
            Just(FocusTarget::Left),
            Just(FocusTarget::Right),
            Just(FocusTarget::Up),
            Just(FocusTarget::Down),
            pane().prop_map(FocusTarget::Pane),
        ]
        .prop_map(|target| Request::Focus {
            instance: None,
            id: 1,
            target
        }),
        (pane(), prop_oneof![Just(-3i16), Just(2), Just(40)]).prop_map(|(pane, delta)| {
            Request::Resize {
                instance: None,
                id: 1,
                pane,
                delta,
            }
        }),
        pane().prop_map(|pane| Request::SendKeys {
            instance: None,
            id: 1,
            pane,
            keys: "x\\n".into(),
            notation: fux::proto::control::KeyNotation::Escapes,
        }),
        (pane(), any::<bool>()).prop_map(|(pane, cells)| Request::Capture {
            if_revision: None,
            instance: None,
            id: 1,
            pane,
            attrs: false,
            scrollback: if cells { 0 } else { 5 },
            max_bytes: 4096,
            format: if cells {
                fux::proto::control::CaptureFormat::Cells
            } else {
                fux::proto::control::CaptureFormat::Text
            },
        }),
        Just(Request::List {
            instance: None,
            id: 1
        }),
        prop_oneof![
            Just(TabAction::New { name: None }),
            Just(TabAction::Next),
            Just(TabAction::Previous),
            (0..4u32).prop_map(|index| TabAction::Select {
                target: fux::proto::control::TabTarget::Index(index)
            }),
            tab().prop_map(|tab| TabAction::Select {
                target: fux::proto::control::TabTarget::Id(tab)
            }),
            tab().prop_map(|tab| TabAction::Rename {
                tab,
                name: "renamed".into(),
            }),
            tab().prop_map(|tab| TabAction::Close { tab }),
        ]
        .prop_map(|action| Request::Tab {
            instance: None,
            id: 1,
            action
        }),
        prop_oneof![
            Just(WorkspaceAction::List),
            Just(WorkspaceAction::New { name: None }),
            name().prop_map(|name| WorkspaceAction::New { name: Some(name) }),
            name().prop_map(|name| WorkspaceAction::Kill { name }),
            name().prop_map(|name| WorkspaceAction::Select { name }),
        ]
        .prop_map(|action| Request::Workspace {
            stream: None,
            instance: None,
            id: 1,
            action
        }),
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
            Just(ManagerRequest::List),
            Just(ManagerRequest::Resolve { name: None }),
            name().prop_map(|name| ManagerRequest::Resolve { name: Some(name) }),
            name().prop_map(|name| ManagerRequest::Kill { name }),
        ]
        .prop_map(Op::Manager),
        3 => (0..13u8, prop::collection::vec(any::<u8>(), 0..64))
            .prop_map(|(pane, bytes)| Op::Output { pane, bytes }),
        1 => (0..13u8).prop_map(|pane| Op::Eof { pane }),
        2 => (0..13u8, 0..3u32).prop_map(|(pane, code)| Op::Exited { pane, code }),
        5 => (0..4u8, prop::bool::weighted(0.8)).prop_map(|(index, ok)| Op::Complete { index, ok }),
        1 => prop_oneof![Just(100u64), Just(4_000), Just(6_000)].prop_map(Op::Advance),
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
        initial: None,
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
                    harness.step(vec![Inbound::Manager { request: action, token: 8 }]);
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
                Op::Advance(ms) => {
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
