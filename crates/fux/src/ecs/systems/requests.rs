//! Request phase: viewer attachments/departures, ordered viewer request queues with creation
//! barriers, control-socket requests and manager requests. Every request resolves public ids at
//! execution time and validates kind, membership and liveness.

use crate::ecs::components::{CreationKind, Pane, PaneState, Selection, Tab, Viewer, Workspace};
use crate::ecs::messages::{
    Effect, Inbound, ManagerAction, ManagerOutcome, Requester, ViewerRequest,
};
use crate::ecs::resources::{
    Clock, Ids, Limits, Registry, ServerIdentity, ShuttingDown, WorkspaceCounter,
};
use crate::ecs::support::{
    Effects, ViewerExit, close_tab, despawn_tab, effect, failed, focus_in_tab, is_member,
    mark_tab_dirty, mark_workspace_dirty, member_tabs, pane_entity, pane_id, pane_in_layout,
    pane_tab, pane_workspace, remove_from_layout, reply, retire, sanitize_notice, tab_entity,
    tab_id, terminate_pane, viewer_entity, viewers_of_workspace, workspace_entity, write_pane,
};
use crate::ecs::systems::creation::{NewPane, reserve_pane, reserve_tab, reserve_workspace};
use crate::ecs::systems::lifecycle::TERMINATE_GRACE_MS;
use crate::ids::{PaneId, ViewerId};
use crate::layout::{Axis, Direction};
use crate::proto::attach::{MouseEvent, ServerMessage, ViewReply};
use crate::proto::control::{
    self, CommandResult, ErrorCode, FocusTarget, PaneSummary, Reply, Request, TabAction,
    TabSummary, WorkspaceAction, WorkspaceSummary,
};
use crate::view::{MouseEncoding, MouseMode, PaneModes, PaneUpdate};
use bevy_ecs::prelude::*;
use std::collections::{BTreeMap, VecDeque};

/// Ingest: viewer arrivals and departures and the shutdown flag. Spawns apply at the sync point
/// before the requests phase, so a viewer's first queued requests find its entity.
/// What an arriving viewer needs: the registry to claim an id in, the limits and clock, the
/// workspace it targets and the viewers already there.
#[derive(bevy_ecs::system::SystemParam)]
pub struct Arrivals<'w, 's> {
    commands: Commands<'w, 's>,
    ids: ResMut<'w, Ids>,
    limits: Res<'w, Limits>,
    clock: Res<'w, Clock>,
    shutting_down: ResMut<'w, ShuttingDown>,
    registry: Res<'w, Registry>,
    workspaces: Query<'w, 's, &'static mut Workspace>,
    viewers: Query<'w, 's, &'static Viewer>,
}

pub fn apply_attachments(
    mut inbound: MessageReader<Inbound>,
    mut arrivals: Arrivals,
    mut exit: ViewerExit,
    mut effects: Effects,
) {
    let Arrivals {
        commands,
        ids,
        limits,
        clock,
        shutting_down,
        registry,
        workspaces,
        viewers,
    } = &mut arrivals;
    // Spawns and despawns apply at the next sync point (`arrivals` is declared before `exit`, so
    // a spawn queued here precedes a despawn queued for the same viewer), so the viewers that
    // arrived or departed in this batch are kept here: the limit counts arrivals, discounts
    // departures, and a departure in the same batch as its arrival still finds the viewer.
    let mut arrived: Vec<(ViewerId, Entity, Entity, String)> = Vec::new();
    let mut departed: Vec<Entity> = Vec::new();
    for message in inbound.read() {
        match message {
            Inbound::ViewerAttached {
                viewer: id,
                workspace,
                rows,
                cols,
            } => {
                let id = *id;
                let refuse = |effects: &mut Effects, message: &str| {
                    effects.emit(Effect::ToViewer {
                        viewer: id,
                        message: ServerMessage::Error {
                            message: message.to_owned(),
                        },
                    });
                    effects.emit(Effect::CloseViewer { viewer: id });
                };
                let Some(entity) = ids.workspace(workspace) else {
                    refuse(&mut effects, "workspace does not exist");
                    continue;
                };
                let Ok(mut target) = workspaces.get_mut(entity) else {
                    refuse(&mut effects, "workspace does not exist");
                    continue;
                };
                if !target.open || target.retiring.is_some() {
                    refuse(&mut effects, "workspace is not accepting viewers");
                    continue;
                }
                let attached = viewers
                    .iter()
                    .filter(|viewer| viewer.workspace == entity && !viewer.detaching)
                    .count()
                    + arrived
                        .iter()
                        .filter(|(_, _, home, _)| *home == entity)
                        .count()
                    - departed
                        .iter()
                        .filter(|gone| {
                            viewers
                                .get(**gone)
                                .is_ok_and(|viewer| viewer.workspace == entity && !viewer.detaching)
                        })
                        .count();
                if attached >= limits.max_viewers {
                    refuse(&mut effects, "viewer limit reached for this workspace");
                    continue;
                }
                target.last_attached = clock.step;
                let viewer = commands
                    .spawn(Viewer {
                        id,
                        workspace: entity,
                        rows: *rows,
                        cols: *cols,
                        selection: target.selection.for_viewer(),
                        queue: VecDeque::new(),
                        barrier: None,
                        generation: 0,
                        layout: Vec::new(),
                        sent: BTreeMap::new(),
                        sent_tabs: Vec::new(),
                        sent_workspace_label: None,
                        dirty: true,
                        pending: false,
                        publish_now: false,
                        input_ms: 0,
                        last_frame_ms: 0,
                        notice: None,
                        after_frame: Vec::new(),
                        detaching: false,
                        exit_sent: false,
                    })
                    .id();
                ids.viewers.insert(id, viewer);
                arrived.push((id, viewer, entity, workspace.clone()));
                effects.emit(Effect::ToViewer {
                    viewer: id,
                    message: ServerMessage::Bindings {
                        bindings: registry.bindings.clone(),
                    },
                });
            }
            Inbound::ViewerGone { viewer: id } => {
                let Some(entity) = ids.viewer(*id) else {
                    continue;
                };
                match viewers.get(entity) {
                    Ok(_) => departed.push(entity),
                    // Arrived in this batch: the spawn is still queued and the despawn queues
                    // behind it.
                    Err(_) => match arrived.iter().position(|(arrived, ..)| arrived == id) {
                        Some(index) => {
                            arrived.swap_remove(index);
                        }
                        None => continue,
                    },
                }
                exit.despawn(ids, entity, *id, &mut effects);
            }
            Inbound::Shutdown => shutting_down.0 = true,
            _ => {}
        }
    }
}

pub fn despawn_viewer(world: &mut World, viewer: Entity) {
    let Some(id) = world.get::<Viewer>(viewer).map(|viewer| viewer.id) else {
        return;
    };
    world.resource_mut::<Ids>().viewers.remove(&id);
    world.despawn(viewer);
    effect(world, Effect::CloseViewer { viewer: id });
}

pub fn apply_requests(world: &mut World) {
    let inbound: Vec<Inbound> = world
        .resource::<Messages<Inbound>>()
        .iter_current_update_messages()
        .filter(|message| {
            matches!(
                message,
                Inbound::ViewerRequest { .. }
                    | Inbound::ControlRequest { .. }
                    | Inbound::Manager { .. }
            )
        })
        .cloned()
        .collect();
    let queue_limit = world.resource::<Limits>().viewer_queue;
    for message in inbound {
        match message {
            Inbound::ViewerRequest { viewer, request } => {
                let Some(entity) = viewer_entity(world, viewer) else {
                    continue;
                };
                let overflow = world
                    .get_mut::<Viewer>(entity)
                    .map(|mut component| {
                        if component.detaching {
                            return false;
                        }
                        if component.queue.len() >= queue_limit {
                            return true;
                        }
                        component.queue.push_back(request);
                        false
                    })
                    .unwrap_or(false);
                if overflow {
                    effect(
                        world,
                        Effect::ToViewer {
                            viewer,
                            message: ServerMessage::Error {
                                message:
                                    "viewer request queue overflowed while a pane was starting"
                                        .into(),
                            },
                        },
                    );
                    effect(world, Effect::CloseViewer { viewer });
                    despawn_viewer(world, entity);
                }
            }
            Inbound::ControlRequest {
                workspace,
                request,
                token,
            } => {
                let requester = Requester::Control(token);
                let id = request.id();
                // A workspace that has not opened yet (initial pane still starting) or is
                // retiring has no control socket of its own; a request naming it can only come
                // through another workspace's socket and must not mutate the reservation.
                let open = workspace_entity(world, &workspace).filter(|entity| {
                    world
                        .get::<Workspace>(*entity)
                        .is_some_and(|workspace| workspace.open && workspace.retiring.is_none())
                });
                match open {
                    Some(entity) => {
                        apply_control(world, requester, Target::Workspace(entity), request);
                    }
                    None => reply(
                        world,
                        requester,
                        failed(id, ErrorCode::NotFound, "workspace is not open"),
                    ),
                }
            }
            Inbound::Manager { action, token } => apply_manager(world, action, token),
            _ => {}
        }
    }
}

/// Applies queued requests in arrival order per viewer, stopping at a creation barrier.
pub fn drain_viewer_queues(world: &mut World) {
    let viewers: Vec<Entity> = {
        let mut viewers: Vec<(ViewerId, Entity)> = world
            .query::<(Entity, &Viewer)>()
            .iter(world)
            .map(|(entity, viewer)| (viewer.id, entity))
            .collect();
        viewers.sort();
        viewers.into_iter().map(|(_, entity)| entity).collect()
    };
    for viewer in viewers {
        while let Some((id, request)) = world.get_mut::<Viewer>(viewer).and_then(|mut component| {
            if component.barrier.is_some() || component.detaching {
                return None;
            }
            let request = component.queue.pop_front()?;
            Some((component.id, request))
        }) {
            apply_viewer_request(world, viewer, id, request);
        }
    }
}

enum Target {
    Viewer(Entity),
    Workspace(Entity),
}

fn apply_viewer_request(world: &mut World, viewer: Entity, id: ViewerId, request: ViewerRequest) {
    let now = world.resource::<Clock>().now_ms;
    if let Some(mut component) = world.get_mut::<Viewer>(viewer) {
        component.input_ms = now;
    }
    match request {
        ViewerRequest::Input(bytes) => {
            let focused = world.get::<Viewer>(viewer).and_then(|viewer| {
                viewer
                    .selection
                    .tab
                    .and_then(|tab| focus_in_tab(world, &viewer.selection, tab))
            });
            let written = focused.is_some_and(|pane| write_pane(world, pane, &bytes));
            if !written && let Some(mut component) = world.get_mut::<Viewer>(viewer) {
                component.notice = Some("No live pane to receive input".into());
                component.dirty = true;
            }
        }
        ViewerRequest::Mouse { event, generation } => apply_mouse(world, viewer, event, generation),
        ViewerRequest::Control(request) => {
            apply_control(
                world,
                Requester::Viewer(id),
                Target::Viewer(viewer),
                request,
            );
        }
        ViewerRequest::View {
            request,
            pane,
            offset,
        } => {
            let workspace = world.get::<Viewer>(viewer).map(|viewer| viewer.workspace);
            let reply = history_view(world, workspace, pane, offset, request);
            effect(
                world,
                Effect::ToViewer {
                    viewer: id,
                    message: ServerMessage::View { reply },
                },
            );
        }
        ViewerRequest::Resize { rows, cols } => {
            if let Some(mut component) = world.get_mut::<Viewer>(viewer) {
                component.rows = rows;
                component.cols = cols;
                component.dirty = true;
            }
        }
        ViewerRequest::Detach => {
            if let Some(mut component) = world.get_mut::<Viewer>(viewer) {
                component.detaching = true;
                component.queue.clear();
            }
        }
    }
}

fn history_view(
    world: &mut World,
    workspace: Option<Entity>,
    pane: PaneId,
    offset: u32,
    request: u64,
) -> ViewReply {
    // Panes of other workspaces are invisible to this attachment, exactly like every other
    // viewer request; a koh gateway authorized for one workspace socket sees only that workspace.
    let unavailable = ViewReply {
        request,
        pane,
        view: None,
        history: 0,
    };
    let Some(mut component) = pane_entity(world, pane)
        .filter(|entity| pane_workspace(world, *entity) == workspace)
        .and_then(|entity| world.get_mut::<Pane>(entity))
        .filter(|component| !matches!(component.state, PaneState::Starting))
    else {
        return unavailable;
    };
    let exit = component.state.exit_code();
    let title = component.published_title.clone();
    let label = component.label.clone();
    let right_click = component.right_click;
    let wanted = usize::try_from(offset).unwrap_or(usize::MAX);
    let (view, history) = component.terminal.with_history_screen(wanted, |screen| {
        let actual = u32::try_from(screen.scrollback()).unwrap_or(u32::MAX);
        (
            PaneUpdate::full_from_screen(screen, &title, actual, exit)
                .ok()
                .map(|mut view| {
                    view.label = label;
                    view.right_click = right_click;
                    view
                }),
            actual,
        )
    });
    // The retained history is at least the offset vt100 accepted; report the larger of the two so
    // a viewer can keep paging until the clamp stops moving.
    let retained = component
        .terminal
        .with_history_screen(usize::MAX, |screen| {
            u32::try_from(screen.scrollback()).unwrap_or(u32::MAX)
        });
    ViewReply {
        request,
        pane,
        view: view.map(Box::new),
        history: retained.max(history),
    }
}

fn apply_mouse(world: &mut World, viewer: Entity, event: MouseEvent, generation: u64) {
    let Some((current, layout, tab, focused)) = world.get::<Viewer>(viewer).map(|component| {
        (
            component.generation,
            component.layout.clone(),
            component.selection.tab,
            component.focused(),
        )
    }) else {
        return;
    };
    if generation != current {
        return;
    }
    let x = event.column.saturating_sub(1);
    let y = event.row.saturating_sub(1);
    let Some((target, rect)) = layout.iter().copied().find(|(_, rect)| rect.contains(x, y)) else {
        return;
    };
    let press = !event.release && !event.motion() && !event.wheel() && event.button() != 3;
    if press
        && focused != Some(target)
        && let Some(tab) = tab
        && let Some(mut component) = world.get_mut::<Viewer>(viewer)
    {
        component.selection.set_focus(tab, target);
        component.dirty = true;
        if let Some(workspace) = world.get::<Viewer>(viewer).map(|viewer| viewer.workspace)
            && let Some(mut workspace) = world.get_mut::<Workspace>(workspace)
        {
            workspace.selection.set_focus(tab, target);
        }
        if let Some(workspace) = world.get::<Viewer>(viewer).map(|viewer| viewer.workspace) {
            crate::ecs::support::refresh_focus_history(world, workspace);
        }
    }
    let content = rect;
    if !content.contains(x, y) {
        return;
    }
    let Some(pane) = world.get::<Pane>(target) else {
        return;
    };
    if !pane.state.accepts_input() {
        return;
    }
    let modes = PaneModes::from_vt100(pane.terminal.screen());
    if let Some(bytes) = encode_mouse(
        event,
        x.saturating_sub(content.x).saturating_add(1),
        y.saturating_sub(content.y).saturating_add(1),
        modes,
    ) {
        write_pane(world, target, &bytes);
    }
}

/// Re-encodes a viewer mouse report for the pane's protocol with pane-relative coordinates.
pub fn encode_mouse(event: MouseEvent, column: u16, row: u16, modes: PaneModes) -> Option<Vec<u8>> {
    let motion = event.motion();
    let button_down = event.button() != 3;
    let report = match modes.mouse_mode {
        MouseMode::None => false,
        MouseMode::Press => !event.release && !motion,
        MouseMode::PressRelease => !motion,
        MouseMode::ButtonMotion => !motion || button_down,
        MouseMode::AnyMotion => true,
    };
    if !report {
        return None;
    }
    if modes.mouse_encoding == MouseEncoding::Sgr {
        return Some(event.sgr(column, row));
    }
    let code = if event.release { 3 } else { event.code };
    let values = [
        u32::from(code) + 32,
        u32::from(column) + 32,
        u32::from(row) + 32,
    ];
    let mut bytes = b"\x1b[M".to_vec();
    match modes.mouse_encoding {
        MouseEncoding::Default => {
            for value in values {
                bytes.push(u8::try_from(value).ok()?);
            }
        }
        MouseEncoding::Utf8 => {
            for value in values {
                let character = char::from_u32(value)?;
                let mut encoded = [0_u8; 4];
                bytes.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
            }
        }
        MouseEncoding::Sgr => return None,
    }
    Some(bytes)
}

struct Context {
    requester: Requester,
    workspace: Entity,
    viewer: Option<Entity>,
}

impl Context {
    fn selection(&self, world: &World) -> Selection {
        self.viewer
            .and_then(|viewer| world.get::<Viewer>(viewer))
            .map(|viewer| viewer.selection.clone())
            .or_else(|| {
                world
                    .get::<Workspace>(self.workspace)
                    .map(|workspace| workspace.selection.clone())
            })
            .unwrap_or_default()
    }

    fn select_tab(&self, world: &mut World, tab: Entity) {
        let focus = focus_in_tab(world, &self.selection(world), tab);
        self.select(world, tab, focus);
    }

    /// The requester (and the workspace default) shows `tab`, focusing `focus` when given.
    fn select(&self, world: &mut World, tab: Entity, focus: Option<Entity>) {
        if let Some(mut component) = world.get_mut::<Tab>(tab)
            && component.zoomed.is_some()
            && focus != component.zoomed
        {
            component.zoomed = None;
            component.layout_changed = true;
            component.layout_generation = component.layout_generation.saturating_add(1);
        }
        if let Some(viewer) = self.viewer
            && let Some(mut component) = world.get_mut::<Viewer>(viewer)
        {
            component.selection.select(tab, focus);
            component.dirty = true;
        }
        if let Some(mut workspace) = world.get_mut::<Workspace>(self.workspace) {
            workspace.selection.select(tab, focus);
        }
        mark_tab_dirty(world, tab);
    }
}

fn apply_control(world: &mut World, requester: Requester, target: Target, request: Request) {
    let id = request.id();
    if let Err(error) = request.validate() {
        return reply(world, requester, control::error_reply(&error));
    }
    if request
        .instance()
        .is_some_and(|instance| instance != world.resource::<ServerIdentity>().instance_nonce)
    {
        return reply(
            world,
            requester,
            failed(
                id,
                ErrorCode::Conflict,
                "server instance changed; rediscover before retrying",
            ),
        );
    }
    let context = match target {
        Target::Viewer(viewer) => {
            let Some(workspace) = world.get::<Viewer>(viewer).map(|viewer| viewer.workspace) else {
                return;
            };
            Context {
                requester,
                workspace,
                viewer: Some(viewer),
            }
        }
        Target::Workspace(workspace) => Context {
            requester,
            workspace,
            viewer: None,
        },
    };
    if context.viewer.is_none()
        && matches!(&request, Request::Layout { action, instance: None, .. } if !action.is_read_only())
    {
        return reply(
            world,
            requester,
            failed(
                id,
                ErrorCode::InvalidRequest,
                "layout changes require the exported server instance",
            ),
        );
    }
    if world.resource::<ShuttingDown>().0
        || world
            .get::<Workspace>(context.workspace)
            .is_none_or(|workspace| workspace.retiring.is_some())
    {
        return reply(
            world,
            requester,
            failed(id, ErrorCode::Conflict, "workspace is shutting down"),
        );
    }
    if let Request::Split {
        stream: Some(expected),
        ..
    }
    | Request::Workspace {
        stream: Some(expected),
        ..
    }
    | Request::FixWorkspace {
        stream: expected, ..
    } = &request
        && world
            .get::<crate::ecs::events::EventLog>(context.workspace)
            .is_none_or(|log| log.cursor().stream != *expected)
    {
        return reply(
            world,
            requester,
            failed(
                id,
                ErrorCode::Conflict,
                "workspace lifetime changed; refresh its identity and retry",
            ),
        );
    }
    let result = match request {
        Request::Split {
            axis,
            target,
            cwd,
            argv,
            env,
            rows,
            columns,
            final_retain_ms,
            fixed_workspace,
            right_click,
            ratio,
            focus,
            ..
        } => split(
            world,
            &context,
            id,
            axis,
            target,
            cwd,
            argv,
            env,
            rows,
            columns,
            final_retain_ms,
            fixed_workspace,
            right_click,
            ratio,
            focus,
        ),
        Request::FixWorkspace { pane, .. } => match pane_in_workspace(world, &context, pane) {
            Some(entity) => {
                if let Some(mut component) = world.get_mut::<Pane>(entity) {
                    component.workspace_pin = crate::ecs::components::WorkspacePin::Explicit;
                }
                mark_workspace_dirty(world, context.workspace);
                Ok(CommandResult::Unit)
            }
            None => Err(failed(
                id,
                ErrorCode::NotFound,
                "pane not found in this workspace",
            )),
        },
        Request::PaneInput {
            pane, right_click, ..
        } => set_right_click(world, &context, id, pane, right_click),
        Request::RenamePane { pane, name, .. } => {
            let result = pane_in_workspace(world, &context, pane)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane no longer exists"));
            result.and_then(|entity| {
                let label = (!name.is_empty()).then_some(name);
                let component = world
                    .get::<Pane>(entity)
                    .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane no longer exists"))?;
                if component.label == label {
                    return Ok(CommandResult::Pane { pane });
                }
                let tab = component.tab;
                let revision = world
                    .get::<Tab>(tab)
                    .and_then(|tab| tab.layout_generation.checked_add(1))
                    .ok_or_else(|| failed(id, ErrorCode::Limit, "layout generation exhausted"))?;
                let changed = world.get_mut::<Pane>(entity).is_some_and(|mut component| {
                    if component.label == label {
                        return false;
                    }
                    component.label = label;
                    true
                });
                if changed {
                    if let Some(mut tab) = world.get_mut::<Tab>(tab) {
                        tab.layout_generation = revision;
                    }
                    mark_workspace_dirty(world, context.workspace);
                }
                Ok(CommandResult::Pane { pane })
            })
        }
        Request::Focus { target, .. } => focus(world, &context, id, target),
        Request::Kill { pane, .. } => kill(world, &context, id, pane),
        Request::Resize { pane, delta, .. } => resize(world, &context, id, pane, delta),
        Request::Layout {
            tab,
            generation,
            action,
            ..
        } => {
            let focus = match &action {
                control::LayoutAction::Transfer {
                    focus: true, pane, ..
                } => Some(*pane),
                _ => None,
            };
            let history = context.selection(world).history;
            let result =
                super::layout_control::apply(world, context.workspace, id, tab, generation, action);
            if result.is_ok()
                && let Some(pane) = focus
                && let Some(entity) = pane_entity(world, pane)
                && let Some(tab) = pane_tab(world, entity)
            {
                if let Some(viewer) = context.viewer
                    && let Some(mut viewer) = world.get_mut::<Viewer>(viewer)
                {
                    // Source fallback is internal to the atomic move, not a navigation visit.
                    viewer.selection.history = history;
                }
                context.select(world, tab, Some(entity));
            }
            result
        }
        Request::SendKeys {
            pane,
            keys,
            notation,
            ..
        } => {
            let bytes =
                control::decode_keys(&keys, notation).map_err(|error| control::error_reply(&error));
            bytes.and_then(|bytes| {
                let entity = pane_in_workspace(world, &context, pane)
                    .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"))?;
                if write_pane(world, entity, &bytes) {
                    Ok(CommandResult::Unit)
                } else {
                    Err(failed(
                        id,
                        ErrorCode::Conflict,
                        "pane is not accepting input",
                    ))
                }
            })
        }
        Request::InputReserve {
            pane, retain_ms, ..
        } => {
            let entity = pane_in_workspace(world, &context, pane)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"));
            entity.and_then(|entity| {
                super::input::reserve(world, context.workspace, entity, retain_ms, id)
            })
        }
        Request::InputSubmit {
            operation, keys, ..
        } => control::decode_key_bytes(&keys)
            .map_err(|error| control::error_reply(&error))
            .and_then(|bytes| super::input::submit(world, context.workspace, operation, bytes, id)),
        Request::InputStatus { operation, .. } => {
            super::input::status(world, context.workspace, operation, id)
        }
        Request::Capture {
            pane,
            attrs,
            scrollback,
            max_bytes,
            format,
            if_revision,
            ..
        } => {
            let entity = pane_in_workspace(world, &context, pane);
            match entity.and_then(|entity| world.get_mut::<Pane>(entity)) {
                Some(mut component) if !matches!(component.state, PaneState::Starting) => {
                    component.refresh();
                    let max_bytes = max_bytes.min(control::MAX_CAPTURE_BYTES);
                    let seq = component.terminal.grid().seq();
                    match format {
                        control::CaptureFormat::Text => {
                            let capture = component.terminal.capture_snapshot(
                                usize::try_from(scrollback).unwrap_or(usize::MAX),
                                attrs,
                                max_bytes,
                                if_revision,
                            );
                            Ok(CommandResult::Capture {
                                capture: Box::new(capture),
                                seq,
                                input_sequence: component.input_sequence,
                            })
                        }
                        control::CaptureFormat::Cells => {
                            let terminal = &component.terminal;
                            let grid = terminal.grid();
                            let revision = terminal.revision();
                            let unchanged = if_revision == Some(revision);
                            let (lines, truncated) = if unchanged {
                                (Vec::new(), false)
                            } else {
                                grid.capture_lines(max_bytes)
                            };
                            let (rows, columns) = grid.size();
                            Ok(CommandResult::Cells {
                                seq,
                                input_sequence: component.input_sequence,
                                revision,
                                rows,
                                columns,
                                cursor: grid.cursor(),
                                title: terminal.title().to_owned(),
                                progress: terminal.progress().map(|p| (p.state, p.percent)),
                                unchanged,
                                truncated,
                                lines,
                            })
                        }
                    }
                }
                _ => Err(failed(id, ErrorCode::NotFound, "pane not found")),
            }
        }
        Request::List { .. } => Ok(CommandResult::Listing {
            instance: world.resource::<ServerIdentity>().instance_nonce.clone(),
            workspaces: vec![summarize(world, &context)],
        }),
        Request::Info { .. } => Ok(CommandResult::Info {
            info: Box::new(server_info(world, Some(context.workspace))),
        }),
        Request::Tab { action, .. } => tab_action(world, &context, id, action),
        Request::Workspace { action, .. } => workspace_action(world, &context, id, action),
        Request::Events { after, .. } => {
            match world
                .get::<crate::ecs::events::EventLog>(context.workspace)
                .and_then(|log| log.replay(after).map(|events| (log.cursor(), events)))
            {
                Some((cursor, events)) => Ok(CommandResult::Events { cursor, events }),
                None => Err(failed(
                    id,
                    ErrorCode::Gap,
                    "event cursor is no longer replayable; relist",
                )),
            }
        }
        Request::Subscribe { .. } => Err(failed(
            id,
            ErrorCode::InvalidRequest,
            "subscriptions are only available on the control socket",
        )),
    };
    match result {
        // A started creation (barrier) replies later, not now.
        Ok(CommandResult::Pane { pane: PaneId(0) }) => {}
        Ok(result) => reply(world, requester, Reply::Completed { id, result }),
        Err(reply_value) => reply(world, requester, reply_value),
    }
}

fn set_right_click(
    world: &mut World,
    context: &Context,
    id: u64,
    pane: PaneId,
    right_click: crate::view::RightClickPolicy,
) -> Result<CommandResult, Reply> {
    let entity = pane_in_workspace(world, context, pane)
        .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane no longer exists"))?;
    let component = world
        .get::<Pane>(entity)
        .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane no longer exists"))?;
    if component.right_click == right_click {
        Ok(CommandResult::Pane { pane })
    } else {
        let tab = component.tab;
        let revision = world
            .get::<Tab>(tab)
            .and_then(|tab| tab.layout_generation.checked_add(1))
            .ok_or_else(|| failed(id, ErrorCode::Limit, "layout generation exhausted"))?;
        if let Some(mut component) = world.get_mut::<Pane>(entity) {
            component.right_click = right_click;
        }
        if let Some(mut component) = world.get_mut::<Tab>(tab) {
            component.layout_generation = revision;
        }
        mark_workspace_dirty(world, context.workspace);
        Ok(CommandResult::Pane { pane })
    }
}

/// A pane id that belongs to the requester's workspace.
fn pane_in_workspace(world: &World, context: &Context, pane: PaneId) -> Option<Entity> {
    let entity = pane_entity(world, pane)?;
    (pane_workspace(world, entity)? == context.workspace).then_some(entity)
}

fn tab_in_workspace(world: &World, context: &Context, tab: crate::ids::TabId) -> Option<Entity> {
    let entity = tab_entity(world, tab)?;
    is_member(world, context.workspace, entity).then_some(entity)
}

#[allow(clippy::too_many_arguments)]
fn split(
    world: &mut World,
    context: &Context,
    id: u64,
    axis: Axis,
    target: Option<PaneId>,
    cwd: Option<std::path::PathBuf>,
    argv: Vec<String>,
    env: Vec<(String, String)>,
    rows: Option<u16>,
    columns: Option<u16>,
    final_retain_ms: u64,
    fixed_workspace: bool,
    right_click: crate::view::RightClickPolicy,
    ratio: u16,
    focus: bool,
) -> Result<CommandResult, Reply> {
    let ratio = std::num::NonZeroU16::new(ratio)
        .filter(|ratio| {
            (crate::layout::MIN_RATIO..=crate::layout::MAX_RATIO).contains(&ratio.get())
        })
        .ok_or_else(|| {
            failed(
                id,
                ErrorCode::InvalidRequest,
                "split ratio must be 500..=9500",
            )
        })?;
    let selection = context.selection(world);
    let target = match target {
        Some(pane) => pane_in_workspace(world, context, pane)
            .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"))?,
        None => selection
            .tab
            .and_then(|tab| focus_in_tab(world, &selection, tab))
            .filter(|pane| pane_in_layout(world, *pane))
            .ok_or_else(|| failed(id, ErrorCode::NotFound, "no focused pane to split"))?,
    };
    let tab =
        pane_tab(world, target).ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"))?;
    let base = world
        .get::<Pane>(target)
        .map(|pane| pane.terminal.size())
        .unwrap_or((24, 80));
    // Estimate the new (second) pane's initial extent using the requested ratio.
    let second = |extent: u16| {
        let usable = extent.saturating_sub(1);
        let first =
            u32::from(usable) * u32::from(ratio.get()) / u32::from(crate::layout::RATIO_SCALE);
        usable.saturating_sub(u16::try_from(first).unwrap_or(usable))
    };
    let initial = match axis {
        Axis::Horizontal => (base.0, second(base.1)),
        Axis::Vertical => (second(base.0), base.1),
    };
    // A requested size wins where no viewer resizes the tab (a headless workspace).
    let size = (rows.unwrap_or(initial.0), columns.unwrap_or(initial.1));
    reserve_pane(
        world,
        context.workspace,
        NewPane {
            argv,
            cwd,
            env,
            requester: context.requester,
            request_id: id,
            final_retain_ms,
            fixed_workspace,
            right_click,
        },
        CreationKind::Split {
            tab,
            target,
            axis,
            ratio,
            focus,
        },
        size,
    )?;
    // The reply follows the spawn report.
    Ok(CommandResult::Pane { pane: PaneId(0) })
}

fn focus(
    world: &mut World,
    context: &Context,
    id: u64,
    target: FocusTarget,
) -> Result<CommandResult, Reply> {
    crate::ecs::support::refresh_focus_history(world, context.workspace);
    let selection = context.selection(world);
    match target {
        FocusTarget::Last => {
            let entity = selection
                .history
                .previous
                .filter(|entity| Some(*entity) != selection.history.current)
                .filter(|entity| pane_in_layout(world, *entity))
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "no previous pane"))?;
            let workspace = pane_workspace(world, entity)
                .filter(|workspace| *workspace == context.workspace || context.viewer.is_some())
                .filter(|workspace| {
                    world
                        .get::<Workspace>(*workspace)
                        .is_some_and(|workspace| workspace.open && workspace.retiring.is_none())
                })
                .ok_or_else(|| {
                    failed(
                        id,
                        ErrorCode::NotFound,
                        "previous pane is unavailable in this workspace",
                    )
                })?;
            let tab = pane_tab(world, entity)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "previous pane is unavailable"))?;
            let pane = pane_id(world, entity)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "previous pane is unavailable"))?;
            if let Some(viewer) = context.viewer {
                check_viewer_admission(world, viewer, workspace, id)?;
                switch_viewer_workspace(world, viewer, workspace);
            }
            Context {
                requester: context.requester,
                workspace,
                viewer: context.viewer,
            }
            .select(world, tab, Some(entity));
            Ok(CommandResult::Pane { pane })
        }
        FocusTarget::Pane(pane) => {
            let entity = pane_in_workspace(world, context, pane)
                .filter(|pane| pane_in_layout(world, *pane))
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"))?;
            let tab = pane_tab(world, entity)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"))?;
            context.select(world, tab, Some(entity));
            Ok(CommandResult::Pane { pane })
        }
        FocusTarget::Next | FocusTarget::Previous => {
            let tab = selection
                .tab
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "no active tab"))?;
            let current = focus_in_tab(world, &selection, tab)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "no focused pane"))?;
            let next = world
                .get::<Tab>(tab)
                .and_then(|component| component.layout.cycle(current, target == FocusTarget::Next))
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "no next pane"))?;
            context.select(world, tab, Some(next));
            Ok(CommandResult::Pane {
                pane: pane_id(world, next).unwrap_or_default(),
            })
        }
        directional => {
            let direction = match directional {
                FocusTarget::Left => Direction::Left,
                FocusTarget::Right => Direction::Right,
                FocusTarget::Up => Direction::Up,
                FocusTarget::Down
                | FocusTarget::Pane(_)
                | FocusTarget::Next
                | FocusTarget::Previous
                | FocusTarget::Last => Direction::Down,
            };
            let tab = selection
                .tab
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "no active tab"))?;
            let current = focus_in_tab(world, &selection, tab)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "no focused pane"))?;
            let next = world
                .get::<Tab>(tab)
                .and_then(|component| {
                    let area = component.area.navigation_area();
                    component.layout.neighbour(current, direction, area)
                })
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "no pane in that direction"))?;
            context.select(world, tab, Some(next));
            let pane = pane_id(world, next).unwrap_or_default();
            Ok(CommandResult::Pane { pane })
        }
    }
}

fn kill(
    world: &mut World,
    context: &Context,
    id: u64,
    pane: PaneId,
) -> Result<CommandResult, Reply> {
    let entity = pane_in_workspace(world, context, pane)
        .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"))?;
    if world
        .get::<Pane>(entity)
        .is_some_and(|pane| matches!(pane.state, PaneState::Starting))
    {
        return Err(failed(id, ErrorCode::Conflict, "pane is still starting"));
    }
    let now = world.resource::<Clock>().now_ms;
    let tab = pane_tab(world, entity);
    remove_from_layout(world, entity);
    // An already exited pane has nothing to wait for: the lifecycle phase publishes pane.closed
    // and despawns it.
    terminate_pane(world, entity, now, TERMINATE_GRACE_MS);
    if let Some(tab) = tab
        && world
            .get::<Tab>(tab)
            .is_some_and(|tab| tab.layout.is_empty())
    {
        close_tab(world, tab, now, TERMINATE_GRACE_MS);
    }
    Ok(CommandResult::Unit)
}

fn resize(
    world: &mut World,
    context: &Context,
    id: u64,
    pane: PaneId,
    delta: i16,
) -> Result<CommandResult, Reply> {
    let entity = pane_in_workspace(world, context, pane)
        .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"))?;
    let tab =
        pane_tab(world, entity).ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"))?;
    let resized = world
        .get_mut::<Tab>(tab)
        .map(|mut component| {
            let ok = component.layout.resize(entity, delta).is_ok();
            component.layout_changed |= ok;
            if ok {
                component.layout_generation = component.layout_generation.saturating_add(1);
            }
            ok
        })
        .unwrap_or(false);
    if resized {
        Ok(CommandResult::Unit)
    } else {
        Err(failed(id, ErrorCode::Conflict, "pane cannot be resized"))
    }
}

fn tab_action(
    world: &mut World,
    context: &Context,
    id: u64,
    action: TabAction,
) -> Result<CommandResult, Reply> {
    let tabs = member_tabs(world, context.workspace);
    let selection = context.selection(world);
    match action {
        TabAction::Reorder { tab, before } => {
            let entity = tab_in_workspace(world, context, tab)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "tab not found"))?;
            let before = before
                .map(|tab| {
                    tab_in_workspace(world, context, tab)
                        .ok_or_else(|| failed(id, ErrorCode::NotFound, "reference tab not found"))
                })
                .transpose()?;
            if !world
                .get_mut::<crate::ecs::components::Tabs>(context.workspace)
                .is_some_and(|mut tabs| tabs.place_before(entity, before))
            {
                return Err(failed(id, ErrorCode::Conflict, "tab order changed"));
            }
            mark_workspace_dirty(world, context.workspace);
            Ok(CommandResult::Tab { tab })
        }
        TabAction::New { name } => {
            let limit = world.resource::<Limits>().max_tabs;
            if tabs.len() >= limit {
                return Err(failed(id, ErrorCode::Limit, "configured tab limit reached"));
            }
            let tab = reserve_tab(world, context.workspace, name)?;
            let size = selection
                .focused()
                .and_then(|pane| world.get::<Pane>(pane))
                .map(|pane| pane.terminal.size())
                .unwrap_or((24, 80));
            if let Err(reply) = reserve_pane(
                world,
                context.workspace,
                NewPane {
                    argv: Vec::new(),
                    cwd: None,
                    env: Vec::new(),
                    requester: context.requester,
                    request_id: id,
                    final_retain_ms: world.resource::<Limits>().final_retain_ms,
                    fixed_workspace: false,
                    right_click: Default::default(),
                },
                CreationKind::NewTab { tab },
                size,
            ) {
                despawn_tab(world, tab);
                return Err(reply);
            }
            Ok(CommandResult::Pane { pane: PaneId(0) })
        }
        TabAction::Next | TabAction::Previous => {
            if tabs.is_empty() {
                return Err(failed(id, ErrorCode::NotFound, "no tabs"));
            }
            let current = selection
                .tab
                .and_then(|tab| tabs.iter().position(|entry| *entry == tab))
                .unwrap_or(0);
            let index = if matches!(action, TabAction::Next) {
                (current + 1) % tabs.len()
            } else {
                current.checked_sub(1).unwrap_or(tabs.len() - 1)
            };
            let tab = *tabs
                .get(index)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "tab not found"))?;
            context.select_tab(world, tab);
            Ok(CommandResult::Tab {
                tab: tab_id(world, tab).unwrap_or_default(),
            })
        }
        TabAction::Select { target } => {
            let entity = match target {
                control::TabTarget::Index(index) => *usize::try_from(index)
                    .ok()
                    .and_then(|index| tabs.get(index))
                    .ok_or_else(|| failed(id, ErrorCode::NotFound, "tab not found"))?,
                control::TabTarget::Id(tab) => tab_in_workspace(world, context, tab)
                    .ok_or_else(|| failed(id, ErrorCode::NotFound, "tab no longer exists"))?,
            };
            context.select_tab(world, entity);
            Ok(CommandResult::Tab {
                tab: tab_id(world, entity).unwrap_or_default(),
            })
        }
        TabAction::Rename { tab, name } => {
            let entity = tab_in_workspace(world, context, tab)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "tab no longer exists"))?;
            if let Some(mut component) = world.get_mut::<Tab>(entity) {
                component.label = name;
            }
            mark_workspace_dirty(world, context.workspace);
            Ok(CommandResult::Tab { tab })
        }
        TabAction::Close { tab } => {
            let entity = tab_in_workspace(world, context, tab)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "tab no longer exists"))?;
            let now = world.resource::<Clock>().now_ms;
            close_tab(world, entity, now, TERMINATE_GRACE_MS);
            Ok(CommandResult::Tab { tab })
        }
    }
}

fn workspace_action(
    world: &mut World,
    context: &Context,
    id: u64,
    action: WorkspaceAction,
) -> Result<CommandResult, Reply> {
    match action {
        WorkspaceAction::Rename { label } => {
            let label = (!label.is_empty()).then_some(label);
            let mut workspace = world
                .get_mut::<Workspace>(context.workspace)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "workspace does not exist"))?;
            let name = workspace.name.clone();
            let changed = workspace.label != label;
            workspace.label = label;
            if changed {
                mark_workspace_dirty(world, context.workspace);
            }
            Ok(CommandResult::Workspace { name })
        }
        WorkspaceAction::List => Ok(CommandResult::Listing {
            instance: world.resource::<ServerIdentity>().instance_nonce.clone(),
            workspaces: list_workspaces(world),
        }),
        WorkspaceAction::New { name } => {
            let name = match name {
                Some(name) => name,
                None => next_workspace_name(world),
            };
            reserve_workspace(world, name, context.requester, id)?;
            Ok(CommandResult::Pane { pane: PaneId(0) })
        }
        WorkspaceAction::Kill { name } => {
            let entity = workspace_entity(world, &name)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "workspace does not exist"))?;
            if entity != context.workspace {
                return Err(failed(
                    id,
                    ErrorCode::Unauthorized,
                    "a workspace connection may only kill its own workspace; use `fux workspace kill`",
                ));
            }
            kill_workspace(world, entity);
            Ok(CommandResult::Workspace { name })
        }
        WorkspaceAction::Select { name } => {
            let viewer = context.viewer.ok_or_else(|| {
                failed(
                    id,
                    ErrorCode::InvalidRequest,
                    "only attached viewers switch workspaces",
                )
            })?;
            let entity = workspace_entity(world, &name)
                .filter(|entity| {
                    world
                        .get::<Workspace>(*entity)
                        .is_some_and(|workspace| workspace.open && workspace.retiring.is_none())
                })
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "workspace does not exist"))?;
            check_viewer_admission(world, viewer, entity, id)?;
            switch_viewer_workspace(world, viewer, entity);
            crate::ecs::support::refresh_focus_history(world, entity);
            Ok(CommandResult::Workspace { name })
        }
    }
}

pub(super) fn check_viewer_admission(
    world: &mut World,
    viewer: Entity,
    workspace: Entity,
    id: u64,
) -> Result<(), Reply> {
    if world
        .get::<Viewer>(viewer)
        .is_some_and(|v| v.workspace == workspace)
    {
        return Ok(());
    }
    let limit = world.resource::<Limits>().max_viewers;
    let occupied = world
        .query::<&Viewer>()
        .iter(world)
        .filter(|other| other.workspace == workspace)
        .count();
    if occupied >= limit {
        return Err(failed(
            id,
            ErrorCode::Limit,
            "that workspace already has the maximum number of viewers",
        ));
    }
    Ok(())
}

/// Moves an attached viewer to another workspace over the same connection.
pub fn switch_viewer_workspace(world: &mut World, viewer: Entity, workspace: Entity) {
    let Some(previous) = world.get::<Viewer>(viewer).map(|viewer| viewer.workspace) else {
        return;
    };
    if previous == workspace {
        return;
    }
    let step = world.resource::<Clock>().step;
    let selection = world
        .get_mut::<Workspace>(workspace)
        .map(|mut component| {
            component.last_attached = step;
            component.selection.clone()
        })
        .unwrap_or_default();
    if let Some(mut component) = world.get_mut::<Viewer>(viewer) {
        let history = component.selection.history;
        component.workspace = workspace;
        component.selection = selection;
        component.selection.history = history;
        component.layout.clear();
        component.sent.clear();
        component.dirty = true;
    }
}

pub fn next_workspace_name(world: &mut World) -> String {
    loop {
        let counter = {
            let mut counter = world.resource_mut::<WorkspaceCounter>();
            counter.0 = counter.0.saturating_add(1);
            counter.0
        };
        let candidate = if counter == 1 {
            "default".to_owned()
        } else {
            format!("ws-{counter}")
        };
        if world.resource::<Ids>().workspace(&candidate).is_none() {
            return candidate;
        }
    }
}

/// Terminates every pane and retires the workspace with exit code 0.
pub fn kill_workspace(world: &mut World, workspace: Entity) {
    let now = world.resource::<Clock>().now_ms;
    for tab in member_tabs(world, workspace) {
        let panes = world
            .get::<Tab>(tab)
            .map(|tab| tab.layout.leaves())
            .unwrap_or_default();
        for pane in panes {
            terminate_pane(world, pane, now, TERMINATE_GRACE_MS);
        }
    }
    retire(world, workspace, now, Some(0));
    mark_workspace_dirty(world, workspace);
}

/// Answers a manager request.
fn manager(world: &mut World, token: u64, outcome: ManagerOutcome) {
    effect(world, Effect::Manager { token, outcome });
}

/// Manager-only inspection: workspace-scoped connections cannot discover foreign panes.
fn locate_pane(
    world: &World,
    instance: &str,
    pane: PaneId,
) -> Result<control::PaneLocation, Reply> {
    if instance != world.resource::<ServerIdentity>().instance_nonce {
        return Err(failed(0, ErrorCode::Conflict, "server instance changed"));
    }
    let entity = pane_entity(world, pane)
        .ok_or_else(|| failed(0, ErrorCode::NotFound, "live pane not found"))?;
    let component = world
        .get::<Pane>(entity)
        .filter(|pane| matches!(pane.state, PaneState::Live { .. } | PaneState::Eof { .. }))
        .ok_or_else(|| failed(0, ErrorCode::NotFound, "live pane not found"))?;
    let pid = component
        .state
        .pid()
        .ok_or_else(|| failed(0, ErrorCode::Conflict, "pane process is unavailable"))?;
    let workspace = world
        .get::<Workspace>(component.routing_workspace)
        .filter(|workspace| workspace.open && workspace.retiring.is_none())
        .ok_or_else(|| failed(0, ErrorCode::NotFound, "workspace unavailable"))?;
    let tab = world
        .get::<Tab>(component.tab)
        .filter(|tab| tab.workspace == component.routing_workspace && tab.layout.contains(entity))
        .ok_or_else(|| failed(0, ErrorCode::NotFound, "pane is no longer in its layout"))?;
    let stream = world
        .get::<crate::ecs::events::EventLog>(component.routing_workspace)
        .map(|log| log.cursor().stream)
        .ok_or_else(|| failed(0, ErrorCode::NotFound, "workspace stream unavailable"))?;
    Ok(control::PaneLocation {
        instance: instance.to_owned(),
        pane,
        pid,
        accepts_input: component.state.accepts_input(),
        workspace: workspace.name.clone(),
        stream,
        origin_workspace: component.workspace_name.clone(),
        origin_stream: component.workspace_stream,
        tab: tab.id,
        layout_generation: tab.layout_generation,
    })
}

fn release_pane_pin(
    world: &mut World,
    instance: &str,
    pane: PaneId,
    pid: u32,
) -> Result<CommandResult, Reply> {
    let location = locate_pane(world, instance, pane)?;
    if !location.accepts_input {
        return Err(failed(
            0,
            ErrorCode::NotFound,
            "pane no longer accepts input",
        ));
    }
    if pid == 0 || location.pid != pid {
        return Err(failed(0, ErrorCode::Conflict, "pane process changed"));
    }
    let entity =
        pane_entity(world, pane).ok_or_else(|| failed(0, ErrorCode::NotFound, "pane missing"))?;
    let mut component = world
        .get_mut::<Pane>(entity)
        .ok_or_else(|| failed(0, ErrorCode::NotFound, "pane missing"))?;
    match component.workspace_pin {
        crate::ecs::components::WorkspacePin::Explicit => {
            return Err(failed(
                0,
                ErrorCode::Conflict,
                "pane has an explicit workspace pin",
            ));
        }
        crate::ecs::components::WorkspacePin::None => return Ok(CommandResult::Unit),
        crate::ecs::components::WorkspacePin::Creation => {}
    }
    component.workspace_pin = crate::ecs::components::WorkspacePin::None;
    let workspace = component.routing_workspace;
    mark_workspace_dirty(world, workspace);
    Ok(CommandResult::Unit)
}

fn apply_manager(world: &mut World, action: ManagerAction, token: u64) {
    match action {
        ManagerAction::ReleasePanePin {
            instance,
            pane,
            pid,
        } => {
            let reply = match release_pane_pin(world, &instance, pane, pid) {
                Ok(result) => Reply::Completed { id: 0, result },
                Err(reply) => reply,
            };
            manager(world, token, ManagerOutcome::ReleasePanePin(reply));
        }
        ManagerAction::InputStatus {
            instance,
            pane,
            operation,
        } => {
            let result = super::input::manager_status(world, &instance, pane, operation);
            let reply = match result {
                Ok(result) => Reply::Completed { id: 0, result },
                Err(reply) => reply,
            };
            manager(world, token, ManagerOutcome::InputStatus(reply));
        }
        ManagerAction::PaneLocation { instance, pane } => {
            let reply = match locate_pane(world, &instance, pane) {
                Ok(location) => Reply::Completed {
                    id: 0,
                    result: CommandResult::PaneLocation { location },
                },
                Err(reply) => reply,
            };
            manager(world, token, ManagerOutcome::PaneLocation(reply));
        }
        ManagerAction::Transfer { transfer } => {
            let result = super::transfer::across_workspaces(world, transfer);
            let reply = match result {
                Ok(result) => Reply::Completed { id: 0, result },
                Err(reply) => reply,
            };
            manager(world, token, ManagerOutcome::Layout(reply));
        }
        ManagerAction::Reorder { name, before } => {
            let mut entries = ordered_workspaces(world);
            let Some(position) = entries.iter().position(|(entry, _)| *entry == name) else {
                return manager(
                    world,
                    token,
                    ManagerOutcome::Failed("workspace not found".into()),
                );
            };
            if before
                .as_ref()
                .is_some_and(|before| !entries.iter().any(|(name, _)| name == before))
            {
                return manager(
                    world,
                    token,
                    ManagerOutcome::Failed("reference workspace not found".into()),
                );
            }
            if before.as_ref() != Some(&name) {
                let entry = entries.remove(position);
                let position = before
                    .as_ref()
                    .and_then(|before| entries.iter().position(|(name, _)| name == before))
                    .unwrap_or(entries.len());
                entries.insert(position, entry);
            }
            world
                .resource_mut::<crate::ecs::resources::WorkspaceOrder>()
                .0 = entries.iter().map(|(_, entity)| *entity).collect();
            manager(
                world,
                token,
                ManagerOutcome::Names(entries.into_iter().map(|(name, _)| name).collect()),
            );
        }
        ManagerAction::Final { instance, pane } => {
            let result = super::final_records::read(world, &instance, pane);
            manager(world, token, ManagerOutcome::Final(result));
        }
        ManagerAction::Create { name } => {
            if world.resource::<ShuttingDown>().0 {
                return manager(
                    world,
                    token,
                    ManagerOutcome::Failed("server is shutting down".into()),
                );
            }
            if let Err(reply) = reserve_workspace(world, name, Requester::Manager(token), 0) {
                let message = match reply {
                    Reply::Failed { error, .. } => error.message,
                    _ => "workspace creation failed".into(),
                };
                manager(world, token, ManagerOutcome::Failed(message));
            }
        }
        ManagerAction::ApplyLayout { expected, archive } => {
            let outcome = match super::layout_archive::apply(world, expected, archive) {
                Ok(archive) => ManagerOutcome::LayoutArchive(archive),
                Err(error) => ManagerOutcome::Failed(error),
            };
            manager(world, token, outcome);
        }
        ManagerAction::ExportLayout => {
            let outcome = match super::layout_archive::export(world) {
                Ok(archive) => ManagerOutcome::LayoutArchive(archive),
                Err(error) => ManagerOutcome::Failed(error),
            };
            manager(world, token, outcome);
        }
        ManagerAction::Catalog => {
            let entries = ordered_workspaces(world)
                .into_iter()
                .filter_map(|(name, entity)| {
                    world
                        .get::<crate::ecs::events::EventLog>(entity)
                        .map(|log| control::WorkspaceRoute {
                            label: world
                                .get::<Workspace>(entity)
                                .and_then(|workspace| workspace.label.clone()),
                            name,
                            stream: log.cursor().stream,
                        })
                })
                .collect();
            let instance = world.resource::<ServerIdentity>().instance_nonce.clone();
            manager(
                world,
                token,
                ManagerOutcome::Catalog(control::WorkspaceCatalog { instance, entries }),
            );
        }
        ManagerAction::List => {
            let names = open_workspace_names(world);
            manager(world, token, ManagerOutcome::Names(names));
        }
        ManagerAction::Kill { name } => match workspace_entity(world, &name) {
            Some(entity) => {
                kill_workspace(world, entity);
                let names = open_workspace_names(world)
                    .into_iter()
                    .filter(|entry| *entry != name)
                    .collect();
                manager(world, token, ManagerOutcome::Names(names));
            }
            None => manager(
                world,
                token,
                ManagerOutcome::Failed("workspace not found".into()),
            ),
        },
        ManagerAction::Info => {
            let info = server_info(world, None);
            manager(world, token, ManagerOutcome::Info(Box::new(info)));
        }
        ManagerAction::Resolve { name } => {
            if world.resource::<ShuttingDown>().0 {
                return manager(
                    world,
                    token,
                    ManagerOutcome::Failed("server is shutting down".into()),
                );
            }
            let requester = Requester::Manager(token);
            let existing = match &name {
                Some(name) => workspace_entity(world, name),
                None => most_recent_workspace(world),
            };
            if let Some(entity) = existing {
                if crate::ecs::systems::creation::workspace_pending(world, entity) {
                    crate::ecs::systems::creation::join_pending_workspace(
                        world, entity, requester, 0,
                    );
                    return;
                }
                let open = world
                    .get::<Workspace>(entity)
                    .filter(|workspace| workspace.open && workspace.retiring.is_none())
                    .map(|workspace| workspace.name.clone());
                let outcome = match open {
                    Some(name) => ManagerOutcome::Attach {
                        stream: world
                            .get::<crate::ecs::events::EventLog>(entity)
                            .map(|log| log.cursor().stream)
                            .unwrap_or(0),
                        name,
                        created: false,
                    },
                    None => ManagerOutcome::Failed("workspace is closing; retry shortly".into()),
                };
                return manager(world, token, outcome);
            }
            let name = name.unwrap_or_else(|| next_workspace_name(world));
            if let Err(reply) = reserve_workspace(world, name, requester, 0) {
                let message = match reply {
                    Reply::Failed { error, .. } => error.message,
                    _ => "workspace creation failed".into(),
                };
                manager(world, token, ManagerOutcome::Failed(message));
            }
        }
    }
}

pub(super) fn ordered_workspaces(world: &mut World) -> Vec<(String, Entity)> {
    let mut entries: Vec<_> = world
        .query::<(Entity, &Workspace)>()
        .iter(world)
        .filter(|(_, workspace)| workspace.open && workspace.retiring.is_none())
        .map(|(entity, workspace)| (workspace.name.clone(), entity))
        .collect();
    let order = &world.resource::<crate::ecs::resources::WorkspaceOrder>().0;
    entries.sort_by(|(a_name, a), (b_name, b)| {
        let rank = |entity| {
            order
                .iter()
                .position(|item| *item == entity)
                .unwrap_or(usize::MAX)
        };
        rank(*a).cmp(&rank(*b)).then_with(|| a_name.cmp(b_name))
    });
    entries
}

fn open_workspace_names(world: &mut World) -> Vec<String> {
    ordered_workspaces(world)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

fn most_recent_workspace(world: &mut World) -> Option<Entity> {
    world
        .query::<(Entity, &Workspace)>()
        .iter(world)
        .filter(|(_, workspace)| workspace.retiring.is_none())
        .max_by_key(|(_, workspace)| workspace.last_attached)
        .map(|(entity, _)| entity)
}

fn list_workspaces(world: &mut World) -> Vec<WorkspaceSummary> {
    let entities: Vec<Entity> = ordered_workspaces(world)
        .into_iter()
        .map(|(_, entity)| entity)
        .collect();
    entities
        .into_iter()
        .map(|workspace| {
            let context = Context {
                requester: Requester::Control(0),
                workspace,
                viewer: None,
            };
            summarize(world, &context)
        })
        .collect()
}

/// What `info` answers: the installed identity, the crate version and the request bounds.
pub fn server_info(world: &World, workspace: Option<Entity>) -> control::ServerInfo {
    let identity = world.resource::<ServerIdentity>();
    let limits = world.resource::<Limits>();
    control::ServerInfo {
        pid: identity.pid,
        instance_nonce: identity.instance_nonce.clone(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        runtime_dir: identity.runtime_dir.clone(),
        workspace: workspace
            .and_then(|entity| world.get::<Workspace>(entity))
            .map(|workspace| workspace.name.clone()),
        limits: control::InfoLimits {
            scrollback_lines: limits.scrollback_lines,
            frame_bytes: control::MAX_FRAME_BYTES,
            capture_bytes: control::MAX_CAPTURE_BYTES,
            key_bytes: control::MAX_KEY_BYTES,
            input_retention_ms: control::MAX_INPUT_RETENTION_MS,
            final_retention_ms: control::MAX_FINAL_RETENTION_MS,
        },
    }
}

fn summarize(world: &mut World, context: &Context) -> WorkspaceSummary {
    let selection = context.selection(world);
    let name = world
        .get::<Workspace>(context.workspace)
        .map(|workspace| workspace.name.clone())
        .unwrap_or_default();
    let tabs = member_tabs(world, context.workspace);
    // A listing reports current sequences: bring every dirty pane's grid up to date first.
    let shown: Vec<Entity> = tabs
        .iter()
        .filter_map(|tab| world.get::<Tab>(*tab))
        .flat_map(|tab| tab.layout.leaves())
        .collect();
    for pane in shown {
        if let Some(mut component) = world.get_mut::<Pane>(pane) {
            component.refresh();
        }
    }
    let viewers =
        u32::try_from(viewers_of_workspace(world, context.workspace).len()).unwrap_or(u32::MAX);
    let tabs = tabs
        .iter()
        .enumerate()
        .filter_map(|(index, tab)| {
            let component = world.get::<Tab>(*tab)?;
            let focused_pane = focus_in_tab(world, &selection, *tab);
            let panes = component
                .layout
                .leaves()
                .into_iter()
                .filter_map(|pane| {
                    let component = world.get::<Pane>(pane)?;
                    let screen = component.terminal.screen();
                    let (row, column) = screen.cursor_position();
                    Some(PaneSummary {
                        right_click: component.right_click,
                        fixed_workspace: component.workspace_pin.is_fixed(),
                        seq: component.terminal.grid().seq(),
                        revision: component.terminal.revision(),
                        input_sequence: component.input_sequence,
                        id: component.id,
                        command: component.argv.clone(),
                        pid: component.state.pid(),
                        cwd: component.cwd.clone(),
                        title: component.published_title.clone(),
                        label: component.label.clone(),
                        geometry: component.rect,
                        focused: focused_pane == Some(pane),
                        cursor: crate::view::Cursor {
                            row,
                            column,
                            hidden: screen.hide_cursor(),
                        },
                        modes: PaneModes::from_vt100(screen),
                        exit_status: component.state.exit_code(),
                    })
                })
                .collect();
            Some(TabSummary {
                id: component.id,
                index: u32::try_from(index).unwrap_or(u32::MAX),
                name: component.label.clone(),
                focused: selection.tab == Some(*tab),
                panes,
            })
        })
        .collect();
    WorkspaceSummary {
        event_cursor: world
            .get::<crate::ecs::events::EventLog>(context.workspace)
            .map(crate::ecs::events::EventLog::cursor)
            .unwrap_or_default(),
        name,
        label: world
            .get::<Workspace>(context.workspace)
            .and_then(|workspace| workspace.label.clone()),
        focused: true,
        viewers,
        tabs,
    }
}

pub fn notice(world: &mut World, viewer: Entity, message: &str) {
    if let Some(mut component) = world.get_mut::<Viewer>(viewer) {
        component.notice = Some(sanitize_notice(message));
        component.dirty = true;
    }
}
