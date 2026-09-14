use super::*;
use crate::proto::control::{LayoutAction, TabAction, WorkspaceAction};
use crate::view::{PaneRect, PaneView, TabEntry};

fn frame() -> Frame {
    let mut parser = vt100::Parser::new(4, 10, 0);
    parser.process(b"hello");
    let view = PaneView::from_screen(parser.screen(), "", 0, None).unwrap_or_default();
    let mut frame = Frame {
        workspace: "default".into(),
        ..Frame::default()
    };
    frame.tabs.push(TabEntry {
        id: TabId(1),
        label: "main".into(),
        layout_generation: 0,
        first_pane: Some(PaneId(1)),
    });
    frame.active_tab = Some(TabId(1));
    frame.focused = Some(PaneId(1));
    frame.layout.push(PaneRect {
        pane: PaneId(1),
        rect: crate::layout::Rect {
            x: 0,
            y: 0,
            width: 12,
            height: 6,
        },
    });
    frame.panes.insert(PaneId(1), view);
    frame
}

fn feed(controller: &mut Controller, bytes: &[u8], frame: &Frame) -> Vec<Request> {
    bytes
        .iter()
        .filter_map(|byte| controller.feed(*byte, frame))
        .collect()
}

pub(super) fn split_frame() -> Frame {
    let mut frame = frame();
    let view = frame.panes.get(&PaneId(1)).cloned().unwrap_or_default();
    frame.panes.insert(PaneId(2), view);
    frame.layout.push(PaneRect {
        pane: PaneId(2),
        rect: crate::layout::Rect {
            x: 13,
            y: 0,
            width: 12,
            height: 6,
        },
    });
    frame.layout_generation = 17;
    frame
}

#[test]
fn successful_field_submission_retires_ownership_without_losing_its_request() {
    let mut frame = frame();
    frame.server_instance = "fixture-owner".into();
    let mut controller = Controller::new(true);
    assert!(controller.enter(Action::RenamePane, &frame));
    controller.panel_bounds = Some(ratatui_core::layout::Rect::new(0, 0, 20, 5));
    let epoch = controller.interaction_epoch();
    let requests = feed(&mut controller, b"\x15renamed\r", &frame);
    assert!(
        matches!(requests.as_slice(), [Request::RenamePane { pane, name, .. }]
            if *pane == PaneId(1) && name == "renamed")
    );
    assert!(!controller.owns_input());
    assert!(controller.panel_bounds.is_none());
    assert_ne!(controller.interaction_epoch(), epoch);
    assert!(controller.take_action().is_none());
    assert!(controller.take_manager_request().is_none());
    assert!(feed(&mut controller, b"next", &frame).is_empty());
}

#[test]
fn destination_chooser_keeps_catalog_lifetime_and_discards_cancelled_input() {
    use crate::proto::control::{WorkspaceCatalog, WorkspaceDestination, WorkspaceRoute};
    let mut frame = frame();
    frame.server_instance = "attached".into();
    frame.viewer = crate::ids::ViewerId(7);
    frame.layout_generation = 12;
    let catalog = || WorkspaceCatalog {
        instance: "attached".into(),
        entries: vec![
            WorkspaceRoute {
                label: None,
                name: "default".into(),
                stream: 1,
            },
            WorkspaceRoute {
                label: None,
                name: "other".into(),
                stream: 8,
            },
        ],
    };
    let mut controller = Controller::new(true);
    assert!(controller.enter(Action::MoveToWorkspace, &frame));
    assert!(matches!(
        controller.take_manager_request(),
        Some(crate::daemon::ManagerRequest::Catalog)
    ));
    assert!(feed(&mut controller, b"\r", &frame).is_empty());
    controller.destinations_loaded(catalog());
    let buffered = controller.take_loading_input();
    assert!(feed(&mut controller, &buffered, &frame).is_empty());
    assert!(
        matches!(controller.take_manager_request(), Some(crate::daemon::ManagerRequest::Transfer {
            transfer: crate::proto::control::WorkspaceTransfer {
                focus: false,
                generation: 12, pane: PaneId(1), follow: Some(crate::ids::ViewerId(7)),
                workspace: WorkspaceDestination::Existing { name, stream: 8 }, ..
            }
        }) if name == "other")
    );
    assert!(controller.enter(Action::MoveToWorkspace, &frame));
    controller.take_manager_request();
    assert!(feed(&mut controller, b"unconfirmed\x1b", &frame).is_empty());
    controller.resolve_escape();
    controller.destinations_loaded(catalog());
    assert!(controller.take_loading_input().is_empty());
    assert!(controller.take_manager_request().is_none());
    assert!(controller.enter(Action::MoveToWorkspace, &frame));
    controller.take_manager_request();
    controller.destinations_loaded(WorkspaceCatalog {
        instance: "replacement".into(),
        ..catalog()
    });
    assert!(!controller.loading_destination());
    assert!(feed(&mut controller, b"\r", &frame).is_empty());
    assert!(controller.take_manager_request().is_none());
    assert!(controller.enter(Action::MoveToWorkspace, &frame));
    controller.take_manager_request();
    frame.layout_generation += 1;
    controller.reconcile(&frame);
    controller.destinations_loaded(catalog());
    assert!(controller.take_manager_request().is_none());
    assert!(!controller.active());
}

#[test]
fn workspace_order_uses_manager_and_cancels_stale_loading() {
    let mut frame = frame();
    let mut controller = Controller::new(true);
    assert!(controller.enter(Action::ReorderWorkspace, &frame));
    controller.workspaces_loaded(
        Ok(vec!["other".into(), "default".into()]
            .into_iter()
            .map(|name| crate::proto::control::WorkspaceRoute {
                name,
                label: None,
                stream: 1,
            })
            .collect()),
        "default",
    );
    assert!(feed(&mut controller, b"\x1b[200~\r\x1b[201~", &frame).is_empty());
    assert!(controller.take_manager_request().is_none());
    assert!(feed(&mut controller, b"\r", &frame).is_empty());
    assert!(
        matches!(controller.take_manager_request(), Some(crate::daemon::ManagerRequest::Reorder {
            name, before: Some(before),
        }) if name == "default" && before == "other")
    );
    assert!(controller.enter(Action::ReorderWorkspace, &frame));
    controller.workspaces_loaded(
        Ok(vec!["other".into()]
            .into_iter()
            .map(|name| crate::proto::control::WorkspaceRoute {
                name,
                label: None,
                stream: 1,
            })
            .collect()),
        "default",
    );
    assert!(feed(&mut controller, b"$", &frame).is_empty());
    assert!(matches!(
        controller.take_manager_request(),
        Some(crate::daemon::ManagerRequest::Reorder { before: None, .. })
    ));
    assert!(controller.enter(Action::ReorderWorkspace, &frame));
    frame.workspace = "changed".into();
    controller.reconcile(&frame);
    controller.workspaces_loaded(
        Ok(vec!["other".into()]
            .into_iter()
            .map(|name| crate::proto::control::WorkspaceRoute {
                name,
                label: None,
                stream: 1,
            })
            .collect()),
        "changed",
    );
    assert!(feed(&mut controller, b"\r", &frame).is_empty());
    assert!(controller.take_manager_request().is_none());
}

#[test]
fn workspace_move_keeps_observed_identity_and_follows_only_its_viewer() {
    let mut frame = frame();
    frame.server_instance = "test-instance".into();
    frame.viewer = crate::ids::ViewerId(7);
    frame.layout_generation = 19;
    let mut controller = Controller::new(true);
    assert!(controller.enter(Action::MoveToNewWorkspace, &frame));
    assert!(feed(&mut controller, b"\r", &frame).is_empty());
    assert!(controller.take_manager_request().is_none());
    assert!(feed(&mut controller, b"\x1b[200~destination\r\x1b[201~", &frame).is_empty());
    assert!(controller.take_manager_request().is_none());
    assert!(feed(&mut controller, b"\r", &frame).is_empty());
    assert!(
        matches!(controller.take_manager_request(), Some(crate::daemon::ManagerRequest::Transfer {
            transfer: crate::proto::control::WorkspaceTransfer {
                focus: false,
                instance, source: TabId(1), generation: 19, pane: PaneId(1), follow: Some(crate::ids::ViewerId(7)),
                workspace: crate::proto::control::WorkspaceDestination::New { name }, ..
            }
        }) if instance == "test-instance" && name == "destination")
    );
    assert!(controller.enter(Action::MoveToNewWorkspace, &frame));
    frame.layout_generation += 1;
    controller.reconcile(&frame);
    assert!(feed(&mut controller, b"destination\r", &frame).is_empty());
    assert!(controller.take_manager_request().is_none());
}

#[test]
fn transfer_chooser_pins_both_revisions_and_ignores_paste() {
    let mut frame = split_frame();
    frame.tabs.push(TabEntry {
        id: TabId(2),
        label: "destination".into(),
        layout_generation: 23,
        first_pane: Some(PaneId(3)),
    });
    let mut controller = Controller::new(true);
    assert!(controller.enter(Action::MoveToTab, &frame));
    assert!(feed(&mut controller, b"\x1b[200~\r\x1b[201~", &frame).is_empty());
    // A new destination revision must not silently change the pending choice.
    if let Some(destination) = frame.tabs.get_mut(1) {
        destination.layout_generation = 24;
    }
    controller.reconcile(&frame);
    assert!(matches!(
        feed(&mut controller, b"\r", &frame).as_slice(),
        [Request::Layout {
            tab: TabId(1),
            generation: Some(17),
            action: LayoutAction::Transfer {
                focus: false,
                pane: PaneId(1),
                destination: PaneDestination::Tab {
                    ratio: 5000,
                    tab: TabId(2),
                    generation: 23,
                    target: PaneId(3),
                },
                side: crate::layout::Direction::Right,
            },
            ..
        }]
    ));
    assert!(controller.enter(Action::MoveToTab, &frame));
    frame.layout_generation += 1;
    controller.reconcile(&frame);
    assert!(feed(&mut controller, b"\r", &frame).is_empty());
    assert!(controller.enter(Action::MoveToTab, &frame));
    frame.tabs.pop();
    assert!(feed(&mut controller, b"\r", &frame).is_empty());
}

#[test]
fn tab_chooser_mouse_selects_reorders_and_cancels_without_forwarding_release() {
    let mut frame = frame();
    for id in 2..=3 {
        frame.tabs.push(TabEntry {
            id: TabId(id),
            label: format!("tab-{id}"),
            layout_generation: 0,
            first_pane: Some(PaneId(id)),
        });
    }
    let mut controller = Controller::new(true);
    let click = MouseEvent {
        code: 0,
        column: 11,
        row: 5,
        release: false,
    };
    assert!(controller.enter(Action::ReorderTab, &frame));
    assert!(matches!(
        controller.mouse(MouseEvent { code: 65, ..click }, &frame),
        MouseDisposition::Local
    ));
    assert!(matches!(controller.mode, Mode::Tabs { selected: 1, .. }));
    controller.entry_regions = vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), 0)];
    assert!(matches!(
        controller.mouse(click, &frame),
        MouseDisposition::Request(Request::Tab {
            action: TabAction::Reorder {
                tab: TabId(1),
                before: Some(TabId(2))
            },
            ..
        })
    ));
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                release: true,
                ..click
            },
            &frame
        ),
        MouseDisposition::Ignore
    ));
    assert!(!controller.capture.left_pending());
    assert!(controller.enter(Action::ChooseTab, &frame));
    controller.entry_regions = vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), 2)];
    assert!(matches!(
        controller.mouse(click, &frame),
        MouseDisposition::Request(Request::Tab {
            action: TabAction::Select {
                target: crate::proto::control::TabTarget::Id(TabId(3))
            },
            ..
        })
    ));
    controller.mouse(
        MouseEvent {
            release: true,
            ..click
        },
        &frame,
    );
    assert!(controller.enter(Action::ReorderTab, &frame));
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                column: 1,
                row: 1,
                ..click
            },
            &frame
        ),
        MouseDisposition::Local
    ));
    assert!(matches!(controller.mode, Mode::Pane));
    assert!(!controller.active());
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                release: true,
                ..click
            },
            &frame
        ),
        MouseDisposition::Ignore
    ));
    assert!(controller.enter(Action::ReorderTab, &frame));
    controller.entry_regions = vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), 0)];
    frame.tabs.remove(0);
    assert!(matches!(
        controller.mouse(click, &frame),
        MouseDisposition::Ignore
    ));
    assert!(!controller.active());
    assert!(controller.capture.left_pending());
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                release: true,
                ..click
            },
            &frame
        ),
        MouseDisposition::Ignore
    ));
    assert!(!controller.capture.left_pending());
    assert!(controller.enter(Action::ChooseTab, &frame));
    controller.entry_regions = vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), 0)];
    assert!(matches!(
        controller.mouse(click, &frame),
        MouseDisposition::Request(Request::Tab {
            action: TabAction::Select {
                target: crate::proto::control::TabTarget::Id(TabId(2))
            },
            ..
        })
    ));
}

#[test]
fn workspace_chooser_mouse_reorders_through_manager() {
    let frame = frame();
    let mut controller = Controller::new(true);
    assert!(controller.enter(Action::ReorderWorkspace, &frame));
    controller.workspaces_loaded(
        Ok(["default", "other", "third"]
            .into_iter()
            .map(|name| crate::proto::control::WorkspaceRoute {
                name: name.into(),
                label: None,
                stream: 1,
            })
            .collect()),
        "default",
    );
    let click = MouseEvent {
        code: 0,
        column: 11,
        row: 5,
        release: false,
    };
    assert!(matches!(
        controller.mouse(MouseEvent { code: 65, ..click }, &frame),
        MouseDisposition::Local
    ));
    assert!(matches!(
        controller.mode,
        Mode::Workspaces { selected: 1, .. }
    ));
    controller.entry_regions = vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), 0)];
    assert!(matches!(
        controller.mouse(click, &frame),
        MouseDisposition::Local
    ));
    assert!(
        matches!(controller.take_manager_request(), Some(crate::daemon::ManagerRequest::Reorder { name, before: Some(before) }) if name == "default" && before == "other")
    );
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                release: true,
                ..click
            },
            &frame
        ),
        MouseDisposition::Ignore
    ));
    assert!(controller.take_manager_request().is_none());
}

#[test]
fn mouse_close_dialogs_confirm_cancel_and_consume_gesture_tails() {
    let mut frame = frame();
    frame.server_instance = "server".into();
    frame.workspace_stream = 7;
    let click = MouseEvent {
        code: 0,
        column: 11,
        row: 5,
        release: false,
    };
    for action in [Action::ClosePane, Action::CloseTab, Action::CloseWorkspace] {
        let mut controller = Controller::new(true);
        for entry in [0, 2, 99] {
            assert!(controller.enter(action, &frame));
            controller.entry_regions = vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), entry)];
            assert!(matches!(
                controller.mouse(click, &frame),
                MouseDisposition::Local
            ));
            assert!(matches!(
                controller.mouse(
                    MouseEvent {
                        release: true,
                        ..click
                    },
                    &frame
                ),
                MouseDisposition::Ignore
            ));
            assert!(!controller.capture.left_pending());
            assert!(controller.take_manager_request().is_none());
            if entry == 0 {
                assert!(controller.active());
            } else {
                assert!(!controller.active());
                assert!(!controller.active());
            }
        }
        assert!(controller.enter(action, &frame));
        assert!(controller.entry_regions.is_empty());
        // A click before the confirmation has painted cannot reuse an old menu row.
        assert!(matches!(
            controller.mouse(click, &frame),
            MouseDisposition::Local
        ));
        assert!(!controller.active());
        controller.mouse(
            MouseEvent {
                release: true,
                ..click
            },
            &frame,
        );
        assert!(controller.enter(action, &frame));
        controller.entry_regions = vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), 1)];
        let disposition = controller.mouse(click, &frame);
        assert!(match action {
            Action::ClosePane => matches!(
                disposition,
                MouseDisposition::Request(Request::Kill {
                    pane: PaneId(1),
                    ..
                })
            ),
            Action::CloseTab => matches!(
                disposition,
                MouseDisposition::Request(Request::Tab {
                    action: TabAction::Close { tab: TabId(1) },
                    ..
                })
            ),
            Action::CloseWorkspace => matches!(
                disposition,
                MouseDisposition::Request(Request::Workspace {
                    stream: Some(7),
                    action: WorkspaceAction::Kill { .. },
                    ..
                })
            ),
            _ => false,
        });
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    release: true,
                    ..click
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        assert!(!controller.capture.left_pending());
    }
    let mut controller = Controller::new(true);
    assert!(controller.enter(Action::ClosePane, &frame));
    controller.entry_regions = vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), 1)];
    frame.panes.clear();
    assert!(matches!(
        controller.mouse(click, &frame),
        MouseDisposition::Local
    ));
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                release: true,
                ..click
            },
            &frame
        ),
        MouseDisposition::Ignore
    ));
    assert!(!controller.active() && !controller.capture.left_pending());
}

#[test]
fn reorder_chooser_places_before_or_last_and_escape_cancels() {
    let mut frame = frame();
    frame.tabs.push(TabEntry {
        id: TabId(2),
        label: "second".into(),
        layout_generation: 0,
        first_pane: Some(PaneId(2)),
    });
    let mut controller = Controller::new(true);
    assert!(controller.enter(Action::ReorderTab, &frame));
    assert!(matches!(
        feed(&mut controller, b"\r", &frame).as_slice(),
        [Request::Tab {
            action: TabAction::Reorder {
                tab: TabId(1),
                before: Some(TabId(2))
            },
            ..
        }]
    ));
    assert!(controller.enter(Action::ReorderTab, &frame));
    assert!(matches!(
        feed(&mut controller, b"$", &frame).as_slice(),
        [Request::Tab {
            action: TabAction::Reorder {
                tab: TabId(1),
                before: None
            },
            ..
        }]
    ));
    assert!(controller.enter(Action::MoveToTab, &frame));
    assert!(feed(&mut controller, b"\x1b", &frame).is_empty());
    controller.resolve_escape();
    assert!(feed(&mut controller, b"\r", &frame).is_empty());
}

#[test]
fn workspace_drop_and_click_keep_dragged_pane_and_consume_release() {
    use crate::proto::control::{WorkspaceCatalog, WorkspaceDestination, WorkspaceRoute};
    let mut frame = split_frame();
    frame.server_instance = "server".into();
    let mut controller = Controller::new(true);
    controller.set_tab_regions(vec![(
        ratatui_core::layout::Rect::new(10, 7, 8, 1),
        TabId(1),
    )]);
    let click = MouseEvent {
        code: 8,
        column: 24,
        row: 2,
        release: false,
    };
    controller.mouse(click, &frame);
    controller.mouse(
        MouseEvent {
            column: 2,
            row: 8,
            release: true,
            ..click
        },
        &frame,
    );
    assert!(matches!(
        controller.take_manager_request(),
        Some(crate::daemon::ManagerRequest::Catalog)
    ));
    controller.destinations_loaded(WorkspaceCatalog {
        instance: "server".into(),
        entries: vec![WorkspaceRoute {
            label: None,
            name: "other".into(),
            stream: 9,
        }],
    });
    controller.entry_regions = vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), 0)];
    let confirm = MouseEvent {
        code: 0,
        column: 12,
        row: 5,
        release: false,
    };
    assert!(matches!(
        controller.mouse(confirm, &frame),
        MouseDisposition::Local
    ));
    assert!(
        matches!(controller.take_manager_request(), Some(crate::daemon::ManagerRequest::Transfer {
            transfer: crate::proto::control::WorkspaceTransfer {
                focus: false, pane: PaneId(2), generation: 17,
                workspace: WorkspaceDestination::Existing {name, stream: 9}, .. }
        }) if name == "other")
    );
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                release: true,
                ..confirm
            },
            &frame
        ),
        MouseDisposition::Ignore
    ));
    assert!(controller.take_manager_request().is_none());
}

#[test]
fn wheel_during_tab_drag_reaches_hidden_destinations_and_wraps() {
    use crate::proto::control::PaneDestination;
    let mut frame = split_frame();
    for id in 2..=4 {
        frame.tabs.push(TabEntry {
            id: TabId(id),
            label: format!("dest-{id}"),
            layout_generation: 20 + u64::from(id),
            first_pane: Some(PaneId(id + 1)),
        });
    }
    let mut controller = Controller::new(true);
    controller.set_tab_regions(vec![(
        ratatui_core::layout::Rect::new(10, 7, 8, 1),
        TabId(2),
    )]);
    let click = MouseEvent {
        code: 8,
        column: 2,
        row: 2,
        release: false,
    };
    let wheel = MouseEvent {
        code: 65,
        column: 12,
        row: 8,
        release: false,
    };
    controller.mouse(click, &frame);
    assert!(matches!(
        controller.mouse(wheel, &frame),
        MouseDisposition::Local
    ));
    assert_eq!(
        controller
            .drag_panel(&frame)
            .and_then(|panel| panel.drop_tab),
        Some(TabId(3))
    );
    controller.mouse(MouseEvent { code: 64, ..wheel }, &frame);
    controller.mouse(MouseEvent { code: 64, ..wheel }, &frame);
    assert_eq!(
        controller
            .drag_panel(&frame)
            .and_then(|panel| panel.drop_tab),
        Some(TabId(4))
    );
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                code: 0,
                release: true,
                ..wheel
            },
            &frame
        ),
        MouseDisposition::Request(Request::Layout {
            action: LayoutAction::Transfer {
                focus: false,
                destination: PaneDestination::Tab {
                    ratio: 5000,
                    tab: TabId(4),
                    generation: 24,
                    target: PaneId(5)
                },
                ..
            },
            ..
        })
    ));
    controller.mouse(click, &frame);
    controller.mouse(wheel, &frame);
    // Moving back into pane content abandons the wheel-selected tab.
    controller.mouse(
        MouseEvent {
            code: 40,
            column: 24,
            row: 3,
            release: false,
        },
        &frame,
    );
    assert_eq!(
        controller
            .drag_panel(&frame)
            .and_then(|panel| panel.drop_tab),
        None
    );
    controller.mouse(wheel, &frame);
    frame.tabs.retain(|tab| tab.id != TabId(3));
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                code: 0,
                release: true,
                ..wheel
            },
            &frame
        ),
        MouseDisposition::Ignore
    ));
}

#[test]
fn pane_tab_drop_keeps_destination_revision_and_cancels_changed_bar() {
    use crate::proto::control::PaneDestination;
    let mut frame = split_frame();
    frame.tabs.push(TabEntry {
        id: TabId(2),
        label: "dest".into(),
        layout_generation: 23,
        first_pane: Some(PaneId(3)),
    });
    let regions = vec![(ratatui_core::layout::Rect::new(10, 7, 8, 1), TabId(2))];
    let click = MouseEvent {
        code: 8,
        column: 2,
        row: 2,
        release: false,
    };
    let drop = MouseEvent {
        code: 0,
        column: 12,
        row: 8,
        release: true,
    };
    let mut controller = Controller::new(true);
    controller.set_tab_regions(regions.clone());
    controller.mouse(click, &frame);
    controller.mouse(
        MouseEvent {
            code: 40,
            release: false,
            ..drop
        },
        &frame,
    );
    assert_eq!(
        controller
            .drag_panel(&frame)
            .and_then(|panel| panel.drop_tab),
        Some(TabId(2))
    );
    assert!(matches!(
        controller.mouse(drop, &frame),
        MouseDisposition::Request(Request::Layout {
            tab: TabId(1),
            generation: Some(17),
            action: LayoutAction::Transfer {
                focus: false,
                pane: PaneId(1),
                destination: PaneDestination::Tab {
                    ratio: 5000,
                    tab: TabId(2),
                    generation: 23,
                    target: PaneId(3)
                },
                ..
            },
            ..
        })
    ));
    controller.mouse(click, &frame);
    for tab in &mut frame.tabs {
        if tab.id == TabId(2) {
            tab.layout_generation += 1;
        }
    }
    assert!(matches!(
        controller.mouse(drop, &frame),
        MouseDisposition::Ignore
    ));
    controller.mouse(click, &frame);
    controller.set_tab_regions(Vec::new());
    assert!(!controller.active());
    assert!(matches!(
        controller.mouse(drop, &frame),
        MouseDisposition::Ignore
    ));
}

#[test]
fn pane_drag_requires_alt_and_commits_only_on_release_with_original_revision() {
    let frame = split_frame();
    let mut controller = Controller::new(true);
    let click = MouseEvent {
        code: 0,
        column: 2,
        row: 2,
        release: false,
    };
    assert!(matches!(
        controller.mouse(click, &frame),
        MouseDisposition::Forward
    ));
    assert!(matches!(
        controller.mouse(MouseEvent { code: 8, ..click }, &frame),
        MouseDisposition::Local
    ));
    assert!(controller.active());
    assert!(feed(&mut controller, b"\x1b[<40;24;3M", &frame).is_empty());
    assert_eq!(
        controller
            .drag_panel(&frame)
            .and_then(|panel| panel.drop_target),
        Some((PaneId(2), crate::layout::Direction::Right))
    );
    let requests = feed(&mut controller, b"\x1b[<8;24;3m", &frame);
    assert!(matches!(
        requests.as_slice(),
        [Request::Layout {
            generation: Some(17),
            action: LayoutAction::Relocate {
                pane: PaneId(1),
                target: PaneId(2),
                side: crate::layout::Direction::Right
            },
            ..
        }]
    ));
    assert!(!controller.active());
}

#[test]
fn a_fresh_press_abandons_an_unreleased_layout_preview() {
    let frame = split_frame();
    let mut controller = Controller::new(true);
    let press = MouseEvent {
        code: 8,
        column: 2,
        row: 2,
        release: false,
    };
    controller.mouse(press, &frame);
    controller.mouse(
        MouseEvent {
            code: 40,
            column: 24,
            ..press
        },
        &frame,
    );
    assert!(controller.drag.is_some());
    // The first gesture lost its release. An ordinary new press must not
    // move the old pane when the second gesture eventually releases.
    assert!(matches!(
        controller.mouse(MouseEvent { code: 0, ..press }, &frame),
        MouseDisposition::Forward
    ));
    assert!(controller.drag.is_none());
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                code: 0,
                column: 24,
                release: true,
                ..press
            },
            &frame
        ),
        MouseDisposition::Forward
    ));
    assert!(!controller.active());
    // A fresh Alt press instead starts its own preview targeting pane 2.
    controller.mouse(press, &frame);
    controller.mouse(
        MouseEvent {
            column: 15,
            ..press
        },
        &frame,
    );
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                code: 0,
                column: 2,
                release: true,
                ..press
            },
            &frame
        ),
        MouseDisposition::Request(Request::Layout {
            action: LayoutAction::Relocate {
                pane: PaneId(2),
                target: PaneId(1),
                ..
            },
            ..
        })
    ));
}

#[test]
fn an_ignored_fresh_right_press_keeps_its_release_owned_across_modes() {
    let mut frame = split_frame();
    frame.server_instance = "attached".into();
    frame
        .panes
        .get_mut(&PaneId(2))
        .unwrap_or_else(|| panic!("pane 2"))
        .modes
        .mouse_mode = MouseMode::AnyMotion;
    let mut controller = Controller::new(true);
    let press = MouseEvent {
        code: 2,
        column: 2,
        row: 2,
        release: false,
    };
    assert!(matches!(
        controller.mouse(press, &frame),
        MouseDisposition::Local
    ));
    assert!(matches!(
        controller.mouse(press, &frame),
        MouseDisposition::Ignore
    ));
    // A keyboard action changes the mode while the ignored second press is
    // still held. Its release must not leak into pane 2's reporting app.
    controller.enter(Action::CopyMode, &frame);
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                column: 15,
                release: true,
                ..press
            },
            &frame
        ),
        MouseDisposition::Ignore
    ));
    assert!(controller.capture.auxiliary().is_none());
}

#[test]
fn cancelled_right_menu_accepts_a_new_press_after_a_lost_release() {
    let mut frame = split_frame();
    frame.server_instance = "attached".into();
    let mut controller = Controller::new(true);
    let press = MouseEvent {
        code: 2,
        column: 2,
        row: 2,
        release: false,
    };
    assert!(matches!(
        controller.mouse(press, &frame),
        MouseDisposition::Local
    ));
    feed(&mut controller, b"\x1b", &frame);
    controller.resolve_escape();
    assert!(!controller.active());
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                column: 15,
                ..press
            },
            &frame
        ),
        MouseDisposition::Local
    ));
    assert!(
        matches!(&controller.mode, Mode::Menu(menu) if menu.target().focused == Some(PaneId(2)))
    );
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                release: true,
                column: 15,
                ..press
            },
            &frame
        ),
        MouseDisposition::Ignore
    ));
}

#[test]
fn layout_drag_ignores_other_buttons_and_keeps_cancelled_tail_captured() {
    let frame = split_frame();
    let mut controller = Controller::new(true);
    let click = MouseEvent {
        code: 8,
        column: 2,
        row: 2,
        release: false,
    };
    controller.mouse(click, &frame);
    for code in [1, 2, 3, 64, 65, 128, 129, 256] {
        for release in [false, true] {
            assert!(matches!(
                controller.mouse(
                    MouseEvent {
                        code,
                        release,
                        column: 24,
                        row: 3,
                    },
                    &frame
                ),
                MouseDisposition::Ignore
            ));
            assert!(controller.active());
        }
    }
    // Releasing Alt before the mouse button still completes the captured left drag.
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                code: 0,
                release: true,
                column: 24,
                row: 3,
            },
            &frame
        ),
        MouseDisposition::Request(Request::Layout {
            action: LayoutAction::Relocate {
                pane: PaneId(1),
                target: PaneId(2),
                ..
            },
            ..
        })
    ));
    controller.mouse(click, &frame);
    controller.end_interaction();
    assert!(
        !controller.active(),
        "drag cancellation must not open the command popup"
    );
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                code: 2,
                release: true,
                ..click
            },
            &frame
        ),
        MouseDisposition::Ignore
    ));
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                code: 40,
                column: 24,
                ..click
            },
            &frame
        ),
        MouseDisposition::Ignore
    ));
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                code: 0,
                release: true,
                ..click
            },
            &frame
        ),
        MouseDisposition::Ignore
    ));
    assert!(matches!(
        controller.mouse(MouseEvent { code: 0, ..click }, &frame),
        MouseDisposition::Forward
    ));
}

#[test]
fn layout_drag_rejects_replaced_server_viewer_or_zoom_even_with_same_revision() {
    for changed in 0..3 {
        let mut frame = split_frame();
        let mut controller = Controller::new(true);
        let click = MouseEvent {
            code: 8,
            column: 2,
            row: 2,
            release: false,
        };
        controller.mouse(click, &frame);
        match changed {
            0 => frame.server_instance.push_str("replacement"),
            1 => frame.viewer = crate::ids::ViewerId(frame.viewer.0 + 1),
            _ => frame.zoomed = Some(PaneId(1)),
        }
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    release: true,
                    column: 24,
                    row: 3,
                    ..click
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        assert!(!controller.active());
    }
}

#[test]
fn border_drag_cancel_and_stale_layout_swallow_the_gesture_tail() {
    let mut frame = split_frame();
    let mut controller = Controller::new(true);
    let click = MouseEvent {
        code: 0,
        column: 13,
        row: 2,
        release: false,
    };
    assert!(matches!(
        controller.mouse(click, &frame),
        MouseDisposition::Local
    ));
    feed(&mut controller, b"\x1b", &frame);
    controller.resolve_escape();
    assert!(!controller.active());
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                release: true,
                column: 18,
                ..click
            },
            &frame
        ),
        MouseDisposition::Ignore
    ));
    controller.mouse(click, &frame);
    frame.layout_generation += 1;
    controller.reconcile(&frame);
    assert!(!controller.active());
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                release: true,
                ..click
            },
            &frame
        ),
        MouseDisposition::Ignore
    ));
    controller.mouse(click, &frame);
    let requests = feed(&mut controller, b"\x1b[<0;18;2m", &frame);
    assert!(matches!(
        requests.as_slice(),
        [Request::Layout {
            generation: Some(18),
            action: LayoutAction::ResizeBorder {
                column: 12,
                row: 1,
                to_column: 17,
                ..
            },
            ..
        }]
    ));
}

#[test]
fn move_and_swap_modes_use_latest_frame_but_keep_original_target() {
    let mut frame = split_frame();
    let mut controller = Controller::new(true);
    assert!(controller.enter(Action::SwapMode, &frame));
    frame.focused = Some(PaneId(2));
    frame.layout_generation = 19;
    assert!(matches!(
        feed(&mut controller, b"\x1b[C", &frame).as_slice(),
        [Request::Layout {
            generation: Some(19),
            action: LayoutAction::SwapDirection {
                pane: PaneId(1),
                direction: crate::layout::Direction::Right
            },
            ..
        }]
    ));
    assert!(controller.enter(Action::MoveMode, &frame));
    assert!(feed(&mut controller, b"\x1b[200~h\x1b[201~", &frame).is_empty());
    assert!(matches!(
        feed(&mut controller, b"h", &frame).as_slice(),
        [Request::Layout {
            action: LayoutAction::MoveDirection {
                pane: PaneId(2),
                direction: crate::layout::Direction::Left
            },
            ..
        }]
    ));
}

#[test]
fn right_click_policy_controls_menu_capture_and_preserves_alt_override() -> Result<(), &'static str>
{
    use crate::view::{MouseMode, RightClickPolicy};
    for policy in [
        RightClickPolicy::Auto,
        RightClickPolicy::Fux,
        RightClickPolicy::Pane,
    ] {
        for reporting in [false, true] {
            for alt in [false, true] {
                let mut frame = split_frame();
                frame.server_instance = "attached".into();
                let pane = frame.panes.get_mut(&PaneId(2)).ok_or("missing pane")?;
                pane.right_click = policy;
                pane.modes.mouse_mode = if reporting {
                    MouseMode::PressRelease
                } else {
                    MouseMode::None
                };
                let mut controller = Controller::new(true);
                let click = MouseEvent {
                    code: if alt { 10 } else { 2 },
                    column: 15,
                    row: 2,
                    release: false,
                };
                let menu = alt
                    || policy == RightClickPolicy::Fux
                    || policy == RightClickPolicy::Auto && !reporting;
                let result = controller.mouse(click, &frame);
                assert_eq!(matches!(result, MouseDisposition::Local), menu);
                assert_eq!(matches!(result, MouseDisposition::Forward), !menu);
                let release = controller.mouse(
                    MouseEvent {
                        release: true,
                        ..click
                    },
                    &frame,
                );
                assert_eq!(matches!(release, MouseDisposition::Ignore), menu);
                assert_eq!(matches!(release, MouseDisposition::Forward), !menu);
            }
        }
    }
    Ok(())
}

#[test]
fn contextual_pane_actions_keep_clicked_target_and_release_ownership() -> Result<(), &'static str> {
    let mut frame = split_frame();
    frame.server_instance = "attached".into();
    let mut controller = Controller::new(true);
    let right = MouseEvent {
        code: 2,
        column: 15,
        row: 2,
        release: false,
    };
    assert!(matches!(
        controller.mouse(right, &frame),
        MouseDisposition::Local
    ));
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                release: true,
                ..right
            },
            &frame
        ),
        MouseDisposition::Ignore
    ));
    assert!(feed(&mut controller, b"\r", &frame).is_empty());
    let (action, target) = controller.take_action().ok_or("menu action")?;
    assert_eq!(action, Action::RenamePane);
    assert_eq!(target.focused, Some(PaneId(2)));
    assert_eq!(
        frame.focused,
        Some(PaneId(1)),
        "menu must not change the real viewer focus"
    );

    assert!(matches!(
        controller.mouse(right, &frame),
        MouseDisposition::Local
    ));
    assert!(feed(&mut controller, b"\x1b", &frame).is_empty());
    controller.resolve_escape();
    assert!(!controller.active());
    assert!(
        !controller.active(),
        "cancelled menu must not hide the captured release behind another popup"
    );
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                release: true,
                ..right
            },
            &frame
        ),
        MouseDisposition::Ignore
    ));
    assert!(matches!(
        controller.mouse(right, &frame),
        MouseDisposition::Local
    ));
    frame.layout_generation += 1;
    controller.reconcile(&frame);
    assert!(!controller.active());
    assert!(controller.take_action().is_none());
    Ok(())
}

#[test]
fn contextual_menu_clicks_disabled_actions_and_application_mouse_override() {
    let mut frame = split_frame();
    frame.server_instance = "attached".into();
    let mut controller = Controller::new(false);
    if let Some(pane) = frame.panes.get_mut(&PaneId(1)) {
        pane.modes.mouse_mode = MouseMode::PressRelease;
    }
    let right = MouseEvent {
        code: 2,
        column: 2,
        row: 2,
        release: false,
    };
    assert!(matches!(
        controller.mouse(right, &frame),
        MouseDisposition::Forward
    ));
    assert!(matches!(
        controller.mouse(MouseEvent { code: 10, ..right }, &frame),
        MouseDisposition::Local
    ));
    controller.mouse(
        MouseEvent {
            code: 10,
            release: true,
            ..right
        },
        &frame,
    );
    controller.entry_regions = vec![(ratatui_core::layout::Rect::new(8, 3, 16, 1), 0)];
    let left = MouseEvent {
        code: 0,
        column: 9,
        row: 4,
        release: false,
    };
    assert!(matches!(
        controller.mouse(left, &frame),
        MouseDisposition::Local
    ));
    assert_eq!(
        controller.take_action().map(|(action, _)| action),
        Some(Action::RenamePane)
    );
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                release: true,
                ..left
            },
            &frame
        ),
        MouseDisposition::Ignore
    ));
    assert!(controller.enter(Action::WorkspaceMenu, &frame));
    assert!(feed(&mut controller, b"\r", &frame).is_empty());
    assert!(
        controller.take_action().is_none(),
        "unavailable manager action dispatched"
    );
    assert!(controller.active());
}

#[test]
fn hidden_tab_menu_targets_its_tab_and_cancels_when_catalog_changes() -> Result<(), &'static str> {
    let mut frame = frame();
    frame.server_instance = "attached".into();
    frame.tabs.push(TabEntry {
        id: TabId(2),
        label: "other".into(),
        layout_generation: 9,
        first_pane: Some(PaneId(2)),
    });
    let mut controller = Controller::new(true);
    controller.tab_regions = vec![(ratatui_core::layout::Rect::new(10, 6, 8, 1), TabId(2))];
    let right = MouseEvent {
        code: 2,
        column: 12,
        row: 7,
        release: false,
    };
    assert!(matches!(
        controller.mouse(right, &frame),
        MouseDisposition::Local
    ));
    controller.mouse(
        MouseEvent {
            release: true,
            ..right
        },
        &frame,
    );
    feed(&mut controller, b"j\r", &frame);
    let (action, target) = controller.take_action().ok_or("tab action")?;
    assert_eq!(action, Action::RenameTab);
    assert_eq!(target.tab, Some(TabId(2)));
    assert_eq!(frame.active_tab, Some(TabId(1)));
    assert!(matches!(
        controller.mouse(right, &frame),
        MouseDisposition::Local
    ));
    frame.tabs.pop();
    controller.reconcile(&frame);
    assert!(!controller.active());
    Ok(())
}

#[test]
fn swap_picker_selects_an_explicit_nonadjacent_target_and_cancels_stale_edits()
-> Result<(), &'static str> {
    let mut frame = split_frame();
    frame.server_instance = "attached".into();
    frame.panes.insert(
        PaneId(3),
        frame.panes.get(&PaneId(1)).cloned().ok_or("pane")?,
    );
    frame.layout.push(PaneRect {
        pane: PaneId(3),
        rect: crate::layout::Rect {
            x: 26,
            y: 0,
            width: 12,
            height: 6,
        },
    });
    let mut controller = Controller::new(true);
    assert!(controller.enter(Action::SwapPane, &frame));
    frame.focused = Some(PaneId(2));
    controller.reconcile(&frame);
    let requests = feed(&mut controller, b"j\r", &frame);
    assert!(matches!(requests.as_slice(), [Request::Layout {
            instance: Some(instance), tab: TabId(1), generation: Some(17),
            action: LayoutAction::Swap { pane: PaneId(1), target: PaneId(3) }, ..
        }] if instance == "attached"));
    frame.focused = Some(PaneId(1));
    assert!(controller.enter(Action::SwapPane, &frame));
    assert!(feed(&mut controller, b"\x1b[200~j\r\x1b[201~", &frame).is_empty());
    assert!(controller.active());
    controller.entry_regions = vec![(ratatui_core::layout::Rect::new(10, 4, 15, 1), 1)];
    let click = MouseEvent {
        code: 0,
        column: 11,
        row: 5,
        release: false,
    };
    assert!(matches!(
        controller.mouse(click, &frame),
        MouseDisposition::Request(Request::Layout {
            action: LayoutAction::Swap {
                pane: PaneId(1),
                target: PaneId(3)
            },
            ..
        })
    ));
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                release: true,
                ..click
            },
            &frame
        ),
        MouseDisposition::Ignore
    ));
    assert!(controller.enter(Action::SwapPane, &frame));
    frame.layout_generation += 1;
    assert!(feed(&mut controller, b"\r", &frame).is_empty());
    assert!(!controller.active());
    assert!(!controller.active());
    Ok(())
}

#[test]
fn workspace_close_confirms_the_captured_lifetime_and_rejects_replacements() {
    let mut frame = frame();
    frame.server_instance = "attached".into();
    frame.workspace_stream = 42;
    let mut controller = Controller::new(false);
    assert!(controller.enter(Action::CloseWorkspace, &frame));
    assert!(feed(&mut controller, b"\x1b[200~y\x1b[201~", &frame).is_empty());
    assert!(controller.active());
    assert_eq!(
        feed(&mut controller, b"y", &frame),
        vec![Request::Workspace {
            id: 0,
            instance: Some("attached".into()),
            stream: Some(42),
            action: WorkspaceAction::Kill {
                name: "default".into()
            },
        }]
    );
    for change in 0..4 {
        assert!(controller.enter(Action::CloseWorkspace, &frame));
        let mut changed = frame.clone();
        match change {
            0 => changed.workspace_stream = 43,
            1 => changed.server_instance = "replacement".into(),
            2 => changed.workspace = "other".into(),
            _ => changed.viewer = crate::ids::ViewerId(999),
        }
        assert!(feed(&mut controller, b"y", &changed).is_empty());
        assert!(!controller.active());
    }
    assert!(controller.enter(Action::CloseWorkspace, &frame));
    controller.capture.adopt(2);
    assert!(feed(&mut controller, b"n", &frame).is_empty());
    assert!(
        !controller.active(),
        "cancel must leave a path for the captured release"
    );
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                code: 2,
                column: 2,
                row: 2,
                release: true
            },
            &frame
        ),
        MouseDisposition::Ignore
    ));
    assert!(controller.capture.auxiliary().is_none());
}

#[test]
fn workspace_rename_edits_labels_and_rejects_stale_lifetimes() {
    let mut frame = split_frame();
    frame.server_instance = "attached".into();
    frame.workspace_stream = 7;
    let mut controller = Controller::new(false);
    assert!(controller.enter(Action::RenameWorkspace, &frame));
    assert!(feed(&mut controller, b"\x1b[200~pasted\r\x1b[201~", &frame).is_empty());
    assert_eq!(
        feed(&mut controller, "\u{15}Build 界\r".as_bytes(), &frame),
        vec![Request::Workspace {
            id: 0,
            instance: Some("attached".into()),
            stream: Some(7),
            action: WorkspaceAction::Rename {
                label: "Build 界".into()
            },
        }]
    );
    assert!(controller.enter(Action::RenameWorkspace, &frame));
    frame.workspace_stream += 1;
    assert!(feed(&mut controller, b"wrong\r", &frame).is_empty());
    assert!(!controller.active());
}

#[test]
fn pane_rename_keeps_its_target_and_cancels_on_identity_changes() {
    let mut frame = split_frame();
    frame.server_instance = "attached".into();
    let mut controller = Controller::new(true);
    assert!(controller.enter(Action::RenamePane, &frame));
    frame.focused = Some(PaneId(2));
    controller.reconcile(&frame);
    assert_eq!(
        feed(&mut controller, "manual界\r".as_bytes(), &frame),
        vec![Request::RenamePane {
            id: 0,
            instance: Some("attached".into()),
            pane: PaneId(1),
            name: "manual界".into(),
        }]
    );
    assert!(controller.enter(Action::RenamePane, &frame));
    assert!(feed(&mut controller, b"\x1b[200~pasted\r\x1b[201~", &frame).is_empty());
    assert!(controller.active(), "pasted newlines must not submit");
    assert_eq!(
        feed(&mut controller, b"\x15\r", &frame),
        vec![Request::RenamePane {
            id: 0,
            instance: Some("attached".into()),
            pane: PaneId(2),
            name: String::new(),
        }]
    );
    for change in 0..3 {
        let mut changed = frame.clone();
        assert!(controller.enter(Action::RenamePane, &frame));
        match change {
            0 => changed.server_instance = "replacement".into(),
            1 => changed.workspace = "other".into(),
            _ => {
                changed.panes.remove(&PaneId(2));
            }
        }
        controller.reconcile(&changed);
        assert!(!controller.active());
        assert!(feed(&mut controller, b"\r", &changed).is_empty());
    }
}

#[test]
fn rename_submits_fragmented_unicode_and_cancels_without_mutation() {
    let frame = frame();
    let mut controller = Controller::new(true);
    assert!(controller.enter(Action::RenameTab, &frame));
    let requests = feed(&mut controller, "\u{15}renamed界\r".as_bytes(), &frame);
    assert_eq!(
        requests,
        vec![Request::Tab {
            instance: None,
            id: 0,
            action: TabAction::Rename {
                tab: TabId(1),
                name: "renamed界".into()
            }
        }]
    );
    assert!(!controller.active());
    assert!(controller.enter(Action::RenameTab, &frame));
    assert!(feed(&mut controller, b"discard\x1b", &frame).is_empty());
    controller.resolve_escape();
    assert!(!controller.active() && !controller.owns_input());
}

#[test]
fn confirmations_carry_the_original_target_and_ignore_paste() {
    let frame = frame();
    let mut controller = Controller::new(true);
    assert!(controller.enter(Action::ClosePane, &frame));
    assert!(feed(&mut controller, b"\x1b[200~y\r\x1b[201~", &frame).is_empty());
    assert!(controller.active(), "pasted confirmation ignored");
    assert_eq!(
        feed(&mut controller, b"y", &frame),
        vec![Request::Kill {
            instance: None,
            id: 0,
            pane: PaneId(1)
        }]
    );
    assert!(controller.enter(Action::CloseTab, &frame));
    assert_eq!(
        feed(&mut controller, b"Y", &frame),
        vec![Request::Tab {
            instance: None,
            id: 0,
            action: TabAction::Close { tab: TabId(1) }
        }]
    );
    // A stale target cancels with feedback when the frame no longer has it.
    assert!(controller.enter(Action::ClosePane, &frame));
    controller.reconcile(&Frame::default());
    assert!(!controller.active());
    assert!(controller.error().is_some());
}

#[test]
fn resize_repeats_with_arrows_and_application_cursor_keys() {
    let frame = frame();
    let mut controller = Controller::new(true);
    assert!(controller.enter(Action::ResizeMode, &frame));
    let requests = feed(&mut controller, b"j\x1b[A\x1bOC\x1bOD\r", &frame);
    let directions: Vec<crate::layout::Direction> = requests
        .iter()
        .filter_map(|request| match request {
            Request::Layout {
                action: LayoutAction::ResizeToward { direction, .. },
                ..
            } => Some(*direction),
            _ => None,
        })
        .collect();
    use crate::layout::Direction::{Down, Left, Right, Up};
    assert_eq!(directions, vec![Down, Up, Right, Left]);
    assert!(!controller.active());
}

#[test]
fn workspace_chooser_replays_buffered_input_and_switches() {
    let frame = frame();
    let mut controller = Controller::new(true);
    assert!(controller.enter(Action::ChooseWorkspace, &frame));
    assert!(feed(&mut controller, b"j\r", &frame).is_empty());
    controller.workspaces_loaded(
        Ok(vec!["default".into(), "other".into()]
            .into_iter()
            .map(|name| crate::proto::control::WorkspaceRoute {
                name,
                label: None,
                stream: 1,
            })
            .collect()),
        "default",
    );
    let replay = controller.take_loading_input();
    assert_eq!(
        feed(&mut controller, &replay, &frame),
        vec![Request::Workspace {
            stream: None,
            instance: None,
            id: 0,
            action: WorkspaceAction::Select {
                name: "other".into()
            }
        }]
    );
    let mut controller = Controller::new(true);
    controller.enter(Action::ChooseWorkspace, &frame);
    controller.workspaces_loaded(Err(anyhow::anyhow!("lookup failed")), "default");
    assert!(!controller.active() && !controller.owns_input());
    let mut controller = Controller::new(true);
    assert!(controller.enter(Action::NewWorkspace, &frame));
    assert_eq!(
        feed(&mut controller, b"proj\r", &frame),
        vec![Request::Workspace {
            stream: None,
            instance: None,
            id: 0,
            action: WorkspaceAction::New {
                name: Some("proj".into())
            }
        }]
    );
}

#[test]
fn normal_mouse_does_not_belong_to_stale_popup_regions() {
    let frame = frame();
    let mut controller = Controller::new(true);
    controller.set_regions(super::super::render::HitRegions {
        tabs: Vec::new(),
        entries: Vec::new(),
        panel: Some(ratatui_core::layout::Rect::new(2, 1, 8, 3)),
    });
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                code: 0,
                column: 4,
                row: 2,
                release: false
            },
            &frame
        ),
        MouseDisposition::Forward
    ));
}

#[test]
fn text_fields_ignore_inside_clicks_and_dismiss_outside_with_release_owned() {
    let mut frame = frame();
    frame.server_instance = "fixture-instance".into();
    frame.workspace_stream = 7;
    for action in [
        Action::RenamePane,
        Action::RenameTab,
        Action::RenameWorkspace,
        Action::NewWorkspace,
        Action::MoveToNewWorkspace,
    ] {
        let mut controller = Controller::new(true);
        assert!(controller.enter(action, &frame));
        controller.set_regions(super::super::render::HitRegions {
            tabs: Vec::new(),
            entries: Vec::new(),
            panel: Some(ratatui_core::layout::Rect::new(2, 1, 8, 3)),
        });
        let click = MouseEvent {
            code: 0,
            column: 4,
            row: 2,
            release: false,
        };
        assert!(!matches!(
            controller.mouse(click, &frame),
            MouseDisposition::Request(_)
        ));
        assert!(controller.active(), "inside click dismissed {action:?}");
        controller.mouse(
            MouseEvent {
                release: true,
                ..click
            },
            &frame,
        );
        assert!(!matches!(
            controller.mouse(
                MouseEvent {
                    column: 1,
                    row: 1,
                    ..click
                },
                &frame
            ),
            MouseDisposition::Request(_)
        ));
        assert!(
            !controller.owns_input(),
            "outside click retained {action:?}"
        );
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    column: 1,
                    row: 1,
                    release: true,
                    ..click
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        assert!(!controller.capture.left_pending());
    }
}

#[test]
fn captured_tab_and_layout_modes_cannot_submit_after_target_loss() {
    for action in [
        Action::RenameTab,
        Action::CloseTab,
        Action::ReorderTab,
        Action::ResizeMode,
        Action::SwapMode,
        Action::MoveMode,
    ] {
        for remove_pane in [false, true] {
            if remove_pane
                && matches!(
                    action,
                    Action::RenameTab | Action::CloseTab | Action::ReorderTab
                )
            {
                continue; // These actions target the tab, not an individual pane.
            }
            let mut original = split_frame();
            original.server_instance = "attached".into();
            let mut controller = Controller::new(true);
            controller.reconcile(&original);
            assert!(controller.enter(action, &original));
            let mut changed = original.clone();
            if remove_pane {
                changed.panes.remove(&PaneId(1));
                changed.focused = Some(PaneId(2));
            } else {
                changed.tabs.clear();
                changed.active_tab = None;
            }
            controller.reconcile(&changed);
            assert!(
                !controller.owns_input(),
                "{action:?} retained a lost target"
            );
            assert!(
                feed(&mut controller, b"y\r\x1b[C", &changed).is_empty(),
                "{action:?} mutated a replacement target"
            );
            assert!(controller.take_action().is_none());
            assert!(controller.take_manager_request().is_none());
        }
    }
}

#[test]
fn workspace_creation_and_menu_discard_replaced_attachment_context() {
    for action in [Action::NewWorkspace, Action::WorkspaceMenu] {
        for replacement in 0..4 {
            let mut original = split_frame();
            original.server_instance = "attached".into();
            original.workspace_stream = 7;
            let mut controller = Controller::new(true);
            controller.reconcile(&original);
            assert!(controller.enter(action, &original));
            let mut changed = original.clone();
            match replacement {
                0 => changed.server_instance = "replacement".into(),
                1 => changed.workspace = "replacement".into(),
                2 => changed.workspace_stream += 1,
                _ => changed.viewer = crate::ids::ViewerId(99),
            }
            controller.reconcile(&changed);
            assert!(
                !controller.owns_input(),
                "{action:?} retained context {replacement}"
            );
            assert!(feed(&mut controller, b"unintended\r", &changed).is_empty());
            assert!(controller.take_action().is_none());
            assert!(controller.take_manager_request().is_none());
        }
    }
}

#[test]
fn vanished_tab_choice_reports_failure_and_escape_returns_normal() {
    let mut frame = split_frame();
    frame.tabs.push(TabEntry {
        id: TabId(2),
        label: "other".into(),
        layout_generation: 1,
        first_pane: Some(PaneId(3)),
    });
    let mut controller = Controller::new(true);
    controller.reconcile(&frame);
    assert!(controller.enter(Action::ChooseTab, &frame));
    assert!(feed(&mut controller, b"j", &frame).is_empty());
    frame.tabs.retain(|tab| tab.id != TabId(2));
    controller.reconcile(&frame);
    assert!(feed(&mut controller, b"\r", &frame).is_empty());
    assert!(
        controller.active(),
        "a chooser can still select a remaining tab"
    );
    assert_eq!(
        controller.error(),
        Some("That tab no longer exists; Esc dismisses.")
    );
    assert!(feed(&mut controller, b"\x1b", &frame).is_empty());
    controller.resolve_escape();
    assert!(!controller.owns_input());
    assert!(controller.take_action().is_none());
    assert!(controller.take_manager_request().is_none());
}

#[test]
fn transient_modes_escape_to_normal_without_forwarding_or_mutating() {
    let mut frame = split_frame();
    frame.server_instance = "fixture-instance".into();
    frame.workspace_stream = 9;
    frame.tabs.push(TabEntry {
        id: TabId(2),
        label: "other".into(),
        layout_generation: 1,
        first_pane: None,
    });
    for action in [
        Action::RenameWorkspace,
        Action::CloseWorkspace,
        Action::PaneMenu,
        Action::TabMenu,
        Action::WorkspaceMenu,
        Action::CopyMode,
        Action::ChooseTab,
        Action::MoveToTab,
        Action::ReorderTab,
        Action::RenamePane,
        Action::RenameTab,
        Action::CloseTab,
        Action::ClosePane,
        Action::ResizeMode,
        Action::SwapMode,
        Action::SwapPane,
        Action::MoveMode,
        Action::NewWorkspace,
        Action::MoveToNewWorkspace,
        Action::MoveToWorkspace,
        Action::ChooseWorkspace,
        Action::ReorderWorkspace,
    ] {
        let mut controller = Controller::new(true);
        controller.reconcile(&frame);
        assert!(
            controller.enter(action, &frame),
            "fixture cannot enter {action:?}"
        );
        assert!(controller.owns_input(), "missing owner for {action:?}");
        assert!(
            feed(&mut controller, b"\x1b", &frame).is_empty(),
            "Escape mutated {action:?}"
        );
        controller.resolve_escape();
        assert!(
            !controller.owns_input() && !controller.active(),
            "Escape retained {action:?}"
        );
        assert!(controller.take_action().is_none());
        assert!(controller.take_copied().is_none());
        let mut filter =
            super::super::input::PrefixFilter::new(crate::commands::ClientBindings::new(1, []));
        assert_eq!(
            filter.feed(b"N"),
            vec![super::super::input::InputEvent::Bytes(vec![b'N'])]
        );
    }
}

#[test]
fn workspace_identity_change_cancels_lookup_and_rejects_its_completion() {
    let mut frame = frame();
    let mut controller = Controller::new(true);
    controller.reconcile(&frame);
    controller.enter(Action::ChooseWorkspace, &frame);
    let epoch = controller.interaction_epoch();
    frame.workspace_stream += 1;
    controller.reconcile(&frame);
    assert!(!controller.owns_input());
    assert!(!controller.workspaces_loaded_for(
        epoch,
        Err(anyhow::anyhow!("stale lookup")),
        "default"
    ));
    assert!(controller.error().is_none());
}

#[test]
fn pending_workspace_lookup_browses_without_replaying_mouse() {
    let frame = frame();
    let mut controller = Controller::new(true);
    controller.enter(Action::ChooseWorkspace, &frame);
    feed(&mut controller, b"j\x1b[<64;3;3M", &frame);
    assert_eq!(controller.local_views().len(), 1);
    assert_eq!(controller.loading_input, b"j");
    feed(&mut controller, b"\x1b", &frame);
    controller.resolve_escape();
    assert!(!controller.owns_input());
    assert!(controller.take_loading_input().is_empty());
    assert_eq!(controller.local_views().len(), 1);
}

#[test]
fn pending_destination_browses_and_dismisses_without_replaying_mouse() {
    let mut frame = frame();
    frame.server_instance = "attached".into();
    let mut controller = Controller::new(true);
    assert!(controller.enter(Action::MoveToWorkspace, &frame));
    controller.take_manager_request();
    feed(&mut controller, b"j\x1b[<64;3;3M", &frame);
    assert_eq!(controller.local_views().len(), 1);
    assert_eq!(controller.loading_input, b"j");
    feed(&mut controller, b"\x1b[<0;3;3M\x1b[<0;3;3m", &frame);
    assert!(!controller.active());
    assert!(controller.take_loading_input().is_empty());
    assert!(controller.take_manager_request().is_none());
    assert_eq!(controller.local_views().len(), 1);
}

#[test]
fn fresh_selection_press_recovers_a_lost_release() {
    for (column, pane, anchor) in [(5, PaneId(1), (1, 4)), (17, PaneId(2), (1, 3))] {
        let frame = split_frame();
        let mut controller = Controller::new(true);
        controller.enter(Action::CopyMode, &frame);
        feed(&mut controller, b"\x1b[<4;2;2M\x1b[<36;3;3M", &frame);
        assert!(controller.selection_dragging());
        let press = MouseEvent {
            code: 4,
            column,
            row: 2,
            release: false,
        };
        assert!(matches!(
            controller.mouse(press, &frame),
            MouseDisposition::Local
        ));
        assert!(matches!(&controller.mode, Mode::Copy(copy)
                if copy.pane() == pane && copy.anchor() == Some(anchor)));
        controller.mouse(
            MouseEvent {
                release: true,
                ..press
            },
            &frame,
        );
        assert!(!controller.selection_dragging());
    }
}

#[test]
fn waiting_command_browses_without_replaying_mouse_and_cancels_buffered_suffix() {
    let frame = frame();
    let mut controller = Controller::new(true);
    controller.wait_for_command();
    let epoch = controller.interaction_epoch();
    feed(&mut controller, b"name\x1b[<64;3;3M", &frame);
    assert_eq!(controller.local_views().len(), 1);
    assert_eq!(controller.loading_input, b"name");
    assert!(controller.waiting_command_ready());
    feed(&mut controller, b"\x1b", &frame);
    controller.resolve_escape();
    assert_ne!(controller.interaction_epoch(), epoch);
    assert!(!controller.owns_input());
    assert!(controller.take_loading_input().is_empty());
    assert_eq!(controller.local_views().len(), 1);
}

#[test]
fn selection_exit_and_clear_own_the_release_then_accept_a_new_drag() {
    for finish in [b"\x1b".as_slice(), b"q", b"c", b"\x1b[<64;3;3M"] {
        let frame = frame();
        let mut controller = Controller::new(true);
        controller.enter(Action::CopyMode, &frame);
        feed(&mut controller, b"\x1b[<0;3;3M", &frame);
        assert!(controller.selection_dragging());
        feed(&mut controller, finish, &frame);
        controller.resolve_escape();
        assert!(!controller.selection_dragging());
        assert!(controller.capture.left_pending());
        let release = MouseEvent {
            code: 0,
            column: 3,
            row: 3,
            release: true,
        };
        assert!(matches!(
            controller.mouse(release, &frame),
            MouseDisposition::Ignore
        ));
        assert!(!controller.capture.left_pending());
        controller.enter(Action::CopyMode, &frame);
        feed(&mut controller, b"\x1b[<0;3;3M", &frame);
        assert!(controller.selection_dragging());
    }
}

#[test]
fn popup_auxiliary_capture_survives_keyboard_command_handoff() {
    for button in [1, 2] {
        let mut frame = split_frame();
        frame
            .panes
            .get_mut(&PaneId(2))
            .unwrap_or_else(|| panic!("pane 2"))
            .modes
            .mouse_mode = MouseMode::AnyMotion;
        let mut popup = super::super::popup::Popup::default();
        let press = MouseEvent {
            code: button,
            column: 15,
            row: 2,
            release: false,
        };
        assert_eq!(popup.mouse(press), super::super::popup::Outcome::Ignore);
        let mut controller = Controller::new(true);
        controller.adopt_popup_capture(popup.take_capture().unwrap_or_else(|| panic!("capture")));
        assert!(controller.enter(Action::CopyMode, &frame));
        assert!(matches!(
            controller.mouse(
                MouseEvent {
                    release: true,
                    ..press
                },
                &frame
            ),
            MouseDisposition::Ignore
        ));
        assert!(matches!(
            controller.mouse(press, &frame),
            MouseDisposition::Forward
        ));
    }
}

#[test]
fn popup_capture_survives_parser_handoff_and_recovers_without_release() {
    let frame = frame();
    let mut controller = Controller::new(true);
    controller.suppress_gesture_tail();
    assert!(controller.enter(Action::CopyMode, &frame));
    assert!(feed(&mut controller, b"\x1b[<0;3;3m", &frame).is_empty());
    assert!(matches!(&controller.mode, Mode::Copy(copy) if !copy.selecting()));
    assert!(feed(&mut controller, b"\x1b[<0;3;3M", &frame).is_empty());
    assert!(matches!(&controller.mode, Mode::Copy(copy) if copy.selecting()));
    controller.end_interaction();
    assert!(controller.capture.left_pending());
    assert!(controller.enter(Action::CopyMode, &frame));
    assert!(feed(&mut controller, b"\x1b[<0;3;3M", &frame).is_empty());
    assert!(matches!(&controller.mode, Mode::Copy(copy) if copy.selecting()));
    assert!(!controller.capture.left_pending());
}

#[test]
fn wheel_browsing_is_passive_and_accepts_another_pane() {
    let frame = split_frame();
    let mut controller = Controller::new(true);
    let wheel = MouseEvent {
        code: 64,
        column: 3,
        row: 3,
        release: false,
    };
    assert!(matches!(
        controller.mouse(wheel, &frame),
        MouseDisposition::Local
    ));
    assert!(
        !controller.owns_input(),
        "wheel must not capture application keys"
    );
    let other = MouseEvent {
        column: 16,
        ..wheel
    };
    assert!(matches!(
        controller.mouse(other, &frame),
        MouseDisposition::Local
    ));
    let first = controller
        .take_read()
        .unwrap_or_else(|| panic!("missing fixture value"));
    let second = controller
        .take_read()
        .unwrap_or_else(|| panic!("missing fixture value"));
    assert_ne!(first.1, second.1);
    assert_eq!((first.2, second.2), (3, 3));
}

#[test]
fn histories_are_memory_bounded_and_exited_apps_do_not_own_the_mouse() {
    let mut frame = split_frame();
    let mut controller = Controller::new(true);
    if let Some(view) = frame.panes.get_mut(&PaneId(1)) {
        view.exit = Some(0);
        view.modes.mouse_mode = MouseMode::AnyMotion;
    }
    assert!(
        Action::CopyMode
            .unavailable(&frame, Target::of(&frame), true)
            .is_none()
    );
    assert!(matches!(
        controller.mouse(
            MouseEvent {
                code: 64,
                column: 3,
                row: 3,
                release: false
            },
            &frame
        ),
        MouseDisposition::Local
    ));
    assert_eq!(controller.local_views().len(), 1);
    controller.enforce_history_budget(1);
    assert!(controller.local_views().is_empty());
    assert!(!controller.owns_input());
}

#[test]
fn overlapping_paste_delimiters_release_a_cancelled_mode() {
    let frame = frame();
    for split in 0..=4 {
        let mut controller = Controller::new(true);
        controller.enter(Action::CopyMode, &frame);
        feed(&mut controller, b"\x1b[200~", &frame);
        let payload = b"a\x1b\x1b[201~";
        let (first, second) = payload.split_at(split);
        feed(&mut controller, first, &frame);
        controller.reconcile(&Frame::default());
        feed(&mut controller, second, &frame);
        assert!(
            !controller.owns_input(),
            "paste ownership stuck at split {split}"
        );
        assert!(controller.take_action().is_none());
    }
}

#[test]
fn clearing_a_selection_releases_capture_without_leaving_copy() {
    let frame = frame();
    let mut controller = Controller::new(true);
    controller.enter(Action::CopyMode, &frame);
    controller.mouse(
        MouseEvent {
            code: 4,
            column: 3,
            row: 2,
            release: false,
        },
        &frame,
    );
    feed(&mut controller, b"c", &frame);
    assert!(matches!(&controller.mode, Mode::Copy(copy) if !copy.dragging() && !copy.selecting()));
}

#[test]
fn cancelled_lookup_cannot_populate_a_reopened_chooser() {
    let frame = frame();
    let mut controller = Controller::new(true);
    controller.enter(Action::ChooseWorkspace, &frame);
    let old = controller.interaction_epoch();
    feed(&mut controller, b"j\x1b", &frame);
    controller.resolve_escape();
    controller.enter(Action::ChooseWorkspace, &frame);
    let new = controller.interaction_epoch();
    assert_ne!(new, old);
    assert!(!controller.workspaces_loaded_for(old, Err(anyhow::anyhow!("old failure")), "default"));
    assert!(matches!(controller.mode, Mode::LoadingWorkspaces { .. }));
    assert!(controller.take_loading_input().is_empty());
    assert!(controller.workspaces_loaded_for(
        new,
        Err(anyhow::anyhow!("current failure")),
        "default"
    ));
    assert!(!controller.active());
    assert_eq!(controller.error(), Some("current failure"));
}

#[test]
fn buffer_switch_discards_history_and_ignores_old_buffer_reply() {
    let mut frame = frame();
    let mut controller = Controller::new(true);
    controller.reconcile(&frame);
    let wheel = MouseEvent {
        code: 64,
        column: 3,
        row: 3,
        release: false,
    };
    controller.mouse(wheel, &frame);
    let (old, pane, _) = controller
        .take_read()
        .unwrap_or_else(|| panic!("history read"));
    frame
        .panes
        .get_mut(&pane)
        .unwrap_or_else(|| panic!("pane"))
        .modes
        .alternate_screen = true;
    controller.reconcile(&frame);
    assert!(controller.local_views().is_empty());
    controller.mouse(MouseEvent { code: 68, ..wheel }, &frame);
    let (new, _, _) = controller
        .take_read()
        .unwrap_or_else(|| panic!("alternate history read"));
    assert_ne!(old, new);
    controller.install_view(ViewReply {
        request: old,
        pane,
        view: None,
        history: 0,
    });
    assert_eq!(controller.local_views().len(), 1);
    controller.enter(Action::CopyMode, &frame);
    frame
        .panes
        .get_mut(&pane)
        .unwrap_or_else(|| panic!("pane"))
        .modes
        .alternate_screen = false;
    controller.reconcile(&frame);
    assert!(!controller.owns_input());
    assert!(controller.local_views().is_empty());
}

#[test]
fn passive_histories_resume_only_the_focused_pane_and_expire_by_identity() {
    let mut frame = split_frame();
    let mut controller = Controller::new(true);
    controller.reconcile(&frame);
    for column in [3, 16] {
        controller.mouse(
            MouseEvent {
                code: 64,
                column,
                row: 3,
                release: false,
            },
            &frame,
        );
    }
    assert_eq!(controller.local_views().len(), 2);
    controller.resume_input(&frame);
    assert_eq!(controller.local_views().len(), 1);
    assert_eq!(
        controller.local_views().first().map(|view| view.pane),
        Some(PaneId(2))
    );
    frame.workspace_stream += 1;
    controller.reconcile(&frame);
    assert!(controller.local_views().is_empty());
}

#[test]
fn copy_parser_forwards_other_application_mouse_and_ends_outside_drag() {
    let mut frame = split_frame();
    frame
        .panes
        .get_mut(&PaneId(2))
        .unwrap_or_else(|| panic!("missing fixture value"))
        .modes
        .mouse_mode = MouseMode::AnyMotion;
    let mut controller = Controller::new(true);
    controller.enter(Action::CopyMode, &frame);
    feed(&mut controller, b"\x1b[<64;16;3M", &frame);
    assert_eq!(
        controller.take_forwarded_mouse().map(|event| event.column),
        Some(16)
    );
    controller.mouse(
        MouseEvent {
            code: 4,
            column: 2,
            row: 2,
            release: false,
        },
        &frame,
    );
    controller.mouse(
        MouseEvent {
            code: 4,
            column: 25,
            row: 20,
            release: true,
        },
        &frame,
    );
    assert!(matches!(&controller.mode, Mode::Copy(copy) if !copy.dragging()));
    controller.mouse(
        MouseEvent {
            code: 4,
            column: 4,
            row: 2,
            release: false,
        },
        &frame,
    );
    assert!(matches!(&controller.mode, Mode::Copy(copy) if copy.anchor() == Some((1, 3))));
}

#[test]
fn copy_escape_dismisses_instead_of_requesting_commands() {
    let frame = frame();
    let mut controller = Controller::new(true);
    controller.enter(Action::CopyMode, &frame);
    feed(&mut controller, b" \x1b", &frame);
    controller.resolve_escape();
    assert!(!controller.owns_input());
    assert!(!controller.active());
}

#[test]
fn copy_mode_selection_and_mouse_routing() {
    let frame = frame();
    let mut controller = Controller::new(true);
    assert!(controller.enter(Action::CopyMode, &frame));
    assert!(feed(&mut controller, b"\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D ", &frame).is_empty());
    feed(&mut controller, b"llll", &frame);
    feed(&mut controller, b"y", &frame);
    assert_eq!(controller.take_copied(), Some("hello".into()));
    assert!(!controller.active());
    // A wheel over a pane whose application does not own the mouse browses locally.
    let wheel = MouseEvent {
        code: 64,
        column: 3,
        row: 3,
        release: false,
    };
    assert!(matches!(
        controller.mouse(wheel, &frame),
        MouseDisposition::Local
    ));
    assert!(!controller.owns_input());
    assert_eq!(controller.take_read().map(|(_, _, offset)| offset), Some(3));
    // A click without shift on the pane is the server's business.
    let mut plain = Controller::new(true);
    let click = MouseEvent {
        code: 0,
        column: 3,
        row: 3,
        release: false,
    };
    assert!(matches!(
        plain.mouse(click, &frame),
        MouseDisposition::Forward
    ));
    // Shift-drag selects locally even when the application owns the mouse.
    let mut owned = frame.clone();
    if let Some(view) = owned.panes.get_mut(&PaneId(1)) {
        view.modes.mouse_mode = MouseMode::AnyMotion;
    }
    let mut shift = Controller::new(true);
    let press = MouseEvent {
        code: 4,
        column: 1,
        row: 1,
        release: false,
    };
    assert!(matches!(
        shift.mouse(press, &owned),
        MouseDisposition::Local
    ));
    let drag = MouseEvent {
        code: 36,
        column: 5,
        row: 1,
        release: false,
    };
    assert!(matches!(shift.mouse(drag, &owned), MouseDisposition::Local));
    let release = MouseEvent {
        code: 4,
        column: 5,
        row: 1,
        release: true,
    };
    assert!(matches!(
        shift.mouse(release, &owned),
        MouseDisposition::Local
    ));
    feed(&mut shift, b"y", &owned);
    assert_eq!(shift.take_copied(), Some("hello".into()));
    assert!(matches!(
        Controller::new(true).mouse(wheel, &owned),
        MouseDisposition::Forward
    ));
}

#[test]
fn canceled_modes_keep_owning_unfinished_pastes() {
    let frame = frame();
    let mut controller = Controller::new(true);
    controller.enter(Action::CopyMode, &frame);
    for byte in b"\x1b[200~ab" {
        assert!(controller.feed(*byte, &frame).is_none());
    }
    controller.reconcile(&Frame::default());
    assert!(!controller.active());
    assert!(
        controller.owns_input(),
        "the paste tail still belongs to the controller"
    );
    for byte in b"t\x01xy\x1b[201~" {
        assert!(controller.feed(*byte, &frame).is_none());
    }
    assert!(!controller.owns_input());
}
