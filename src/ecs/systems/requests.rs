//! Request phase: viewer attachments/departures, ordered viewer request queues with creation
//! barriers, control-socket requests and manager requests. Every request resolves public ids at
//! execution time and validates kind, membership and liveness.

use crate::ecs::components::{CreationKind, Pane, PaneState, Selection, Tab, Viewer, Workspace};
use crate::ecs::messages::{
    Effect, Inbound, ManagerAction, ManagerOutcome, Requester, ViewerRequest,
};
use crate::ecs::resources::{Clock, Ids, Limits, ShuttingDown, WorkspaceCounter};
use crate::ecs::support::{
    Effects, ViewerExit, close_tab, despawn_tab, effect, event, failed, focus_in_tab, is_member,
    mark_tab_dirty, mark_workspace_dirty, member_tabs, pane_entity, pane_id, pane_in_layout,
    pane_tab, pane_workspace, remove_from_layout, reply, retire, sanitize_notice, tab_entity,
    tab_id, terminate_pane, viewer_entity, viewers_of_workspace, workspace_entity, write_pane,
};
use crate::ecs::systems::creation::{NewPane, reserve_pane, reserve_tab, reserve_workspace};
use crate::ecs::systems::lifecycle::TERMINATE_GRACE_MS;
use crate::ids::{PaneId, ViewerId};
use crate::layout::{Axis, Direction, Rect};
use crate::proto::attach::{MouseEvent, ServerMessage, ViewReply};
use crate::proto::control::{
    self, CommandResult, ErrorCode, Event, FocusTarget, PaneSummary, Reply, Request, TabAction,
    TabSummary, WorkspaceAction, WorkspaceSummary,
};
use crate::view::{MouseEncoding, MouseMode, PaneModes, PaneView};
use bevy_ecs::prelude::*;
use std::collections::VecDeque;

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
                        selection: target.selection.clone(),
                        queue: VecDeque::new(),
                        barrier: None,
                        generation: 0,
                        layout: Vec::new(),
                        dirty: true,
                        notice: None,
                        after_frame: Vec::new(),
                        detaching: false,
                        exit_sent: false,
                    })
                    .id();
                ids.viewers.insert(id, viewer);
                arrived.push((id, viewer, entity, workspace.clone()));
                effects.event(
                    workspace,
                    Event::ClientAttached {
                        id: 0,
                        client: id.0,
                    },
                );
            }
            Inbound::ViewerGone { viewer: id } => {
                let Some(entity) = ids.viewer(*id) else {
                    continue;
                };
                let name = match viewers.get(entity) {
                    Ok(viewer) => {
                        departed.push(entity);
                        workspaces
                            .get(viewer.workspace)
                            .map(|workspace| workspace.name.clone())
                            .unwrap_or_default()
                    }
                    // Arrived in this batch: the spawn is still queued and the despawn queues
                    // behind it.
                    Err(_) => match arrived.iter().position(|(arrived, ..)| arrived == id) {
                        Some(index) => arrived.swap_remove(index).3,
                        None => continue,
                    },
                };
                exit.despawn(ids, entity, *id, &name, &mut effects);
            }
            Inbound::Shutdown => shutting_down.0 = true,
            _ => {}
        }
    }
}

pub fn despawn_viewer(world: &mut World, viewer: Entity) {
    let Some((id, workspace)) = world
        .get::<Viewer>(viewer)
        .map(|viewer| (viewer.id, viewer.workspace))
    else {
        return;
    };
    world.resource_mut::<Ids>().viewers.remove(&id);
    world.despawn(viewer);
    effect(world, Effect::CloseViewer { viewer: id });
    event(
        world,
        workspace,
        Event::ClientDetached {
            id: 0,
            client: id.0,
        },
    );
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
    match request {
        ViewerRequest::Input(bytes) => {
            let focused = world
                .get::<Viewer>(viewer)
                .and_then(|viewer| viewer.focused());
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
    let wanted = usize::try_from(offset).unwrap_or(usize::MAX);
    let (view, history) = component.terminal.with_history_screen(wanted, |screen| {
        let actual = u32::try_from(screen.scrollback()).unwrap_or(u32::MAX);
        (
            PaneView::from_screen(screen, &title, actual, exit).ok(),
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
    let result = match request {
        Request::Split {
            axis,
            target,
            cwd,
            argv,
            ..
        } => split(world, &context, id, axis, target, cwd, argv),
        Request::New { cwd, argv, .. } => {
            split(world, &context, id, Axis::Horizontal, None, cwd, argv)
        }
        Request::Focus { target, .. } => focus(world, &context, id, target),
        Request::Kill { pane, .. } => kill(world, &context, id, pane),
        Request::Resize { pane, delta, .. } => resize(world, &context, id, pane, delta),
        Request::SendKeys { pane, keys, .. } => {
            let bytes =
                control::decode_key_bytes(&keys).map_err(|error| control::error_reply(&error));
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
        Request::Capture {
            pane,
            attrs,
            scrollback,
            max_bytes,
            ..
        } => {
            let entity = pane_in_workspace(world, &context, pane);
            match entity.and_then(|entity| world.get_mut::<Pane>(entity)) {
                Some(mut component) if !matches!(component.state, PaneState::Starting) => {
                    let text = component.terminal.capture(
                        usize::try_from(scrollback).unwrap_or(usize::MAX),
                        attrs,
                        max_bytes.min(control::MAX_CAPTURE_BYTES),
                    );
                    Ok(CommandResult::Capture { text })
                }
                _ => Err(failed(id, ErrorCode::NotFound, "pane not found")),
            }
        }
        Request::List { .. } => Ok(CommandResult::Listing {
            workspaces: vec![summarize(world, &context)],
        }),
        Request::Tab { action, .. } => tab_action(world, &context, id, action),
        Request::Workspace { action, .. } => workspace_action(world, &context, id, action),
        Request::Subscribe { .. } => Err(failed(
            id,
            ErrorCode::InvalidRequest,
            "subscriptions are only available on the control socket",
        )),
    };
    match result {
        Ok(CommandResult::Pane { pane: PaneId(0) }) => {}
        Ok(result) => reply(world, requester, Reply::Completed { id, result }),
        Err(reply_value) => reply(world, requester, reply_value),
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

fn split(
    world: &mut World,
    context: &Context,
    id: u64,
    axis: Axis,
    target: Option<PaneId>,
    cwd: Option<std::path::PathBuf>,
    argv: Vec<String>,
) -> Result<CommandResult, Reply> {
    let selection = context.selection(world);
    let target = match target {
        Some(pane) => pane_in_workspace(world, context, pane)
            .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"))?,
        None => selection
            .focused()
            .filter(|pane| pane_in_layout(world, *pane))
            .ok_or_else(|| failed(id, ErrorCode::NotFound, "no focused pane to split"))?,
    };
    let tab =
        pane_tab(world, target).ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"))?;
    let size = world
        .get::<Pane>(target)
        .map(|pane| pane.terminal.size())
        .unwrap_or((24, 80));
    // Give the new pane roughly half of the split pane's area from the start.
    let size = match axis {
        Axis::Horizontal => (size.0, size.1.saturating_sub(1) / 2),
        Axis::Vertical => (size.0.saturating_sub(1) / 2, size.1),
    };
    reserve_pane(
        world,
        context.workspace,
        NewPane {
            argv,
            cwd,
            requester: context.requester,
            request_id: id,
        },
        CreationKind::Split { tab, target, axis },
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
    let selection = context.selection(world);
    match target {
        FocusTarget::Pane(pane) => {
            let entity = pane_in_workspace(world, context, pane)
                .filter(|pane| pane_in_layout(world, *pane))
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"))?;
            let tab = pane_tab(world, entity)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "pane not found"))?;
            context.select(world, tab, Some(entity));
            Ok(CommandResult::Pane { pane })
        }
        directional => {
            let direction = match directional {
                FocusTarget::Left => Direction::Left,
                FocusTarget::Right => Direction::Right,
                FocusTarget::Up => Direction::Up,
                FocusTarget::Down | FocusTarget::Pane(_) => Direction::Down,
            };
            let tab = selection
                .tab
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "no active tab"))?;
            let current = focus_in_tab(world, &selection, tab)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "no focused pane"))?;
            let next = world
                .get::<Tab>(tab)
                .and_then(|component| {
                    let area = if component.area.width == 0 || component.area.height == 0 {
                        Rect {
                            x: 0,
                            y: 0,
                            width: 1000,
                            height: 1000,
                        }
                    } else {
                        component.area
                    };
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
                    requester: context.requester,
                    request_id: id,
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
        TabAction::Select { index } => {
            let tab = *usize::try_from(index)
                .ok()
                .and_then(|index| tabs.get(index))
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "tab not found"))?;
            context.select_tab(world, tab);
            Ok(CommandResult::Tab {
                tab: tab_id(world, tab).unwrap_or_default(),
            })
        }
        TabAction::SelectId { tab } => {
            let entity = tab_in_workspace(world, context, tab)
                .ok_or_else(|| failed(id, ErrorCode::NotFound, "tab no longer exists"))?;
            context.select_tab(world, entity);
            Ok(CommandResult::Tab { tab })
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
        WorkspaceAction::List => Ok(CommandResult::Listing {
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
            let limit = world.resource::<Limits>().max_viewers;
            let occupied = world
                .query::<&Viewer>()
                .iter(world)
                .filter(|other| other.workspace == entity)
                .count();
            if occupied >= limit {
                return Err(failed(
                    id,
                    ErrorCode::Limit,
                    "that workspace already has the maximum number of viewers",
                ));
            }
            switch_viewer_workspace(world, viewer, entity);
            Ok(CommandResult::Workspace { name })
        }
    }
}

/// Moves an attached viewer to another workspace over the same connection.
pub fn switch_viewer_workspace(world: &mut World, viewer: Entity, workspace: Entity) {
    let Some((id, previous)) = world
        .get::<Viewer>(viewer)
        .map(|viewer| (viewer.id, viewer.workspace))
    else {
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
        component.workspace = workspace;
        component.selection = selection;
        component.layout.clear();
        component.dirty = true;
    }
    event(
        world,
        previous,
        Event::ClientDetached {
            id: 0,
            client: id.0,
        },
    );
    event(
        world,
        workspace,
        Event::ClientAttached {
            id: 0,
            client: id.0,
        },
    );
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

fn apply_manager(world: &mut World, action: ManagerAction, token: u64) {
    match action {
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

fn open_workspace_names(world: &mut World) -> Vec<String> {
    let mut names: Vec<String> = world
        .query::<&Workspace>()
        .iter(world)
        .filter(|workspace| workspace.open && workspace.retiring.is_none())
        .map(|workspace| workspace.name.clone())
        .collect();
    names.sort();
    names
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
    let entities: Vec<Entity> = {
        let mut entries: Vec<(String, Entity)> = world
            .query::<(Entity, &Workspace)>()
            .iter(world)
            .filter(|(_, workspace)| workspace.open && workspace.retiring.is_none())
            .map(|(entity, workspace)| (workspace.name.clone(), entity))
            .collect();
        entries.sort();
        entries.into_iter().map(|(_, entity)| entity).collect()
    };
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

fn summarize(world: &mut World, context: &Context) -> WorkspaceSummary {
    let selection = context.selection(world);
    let name = world
        .get::<Workspace>(context.workspace)
        .map(|workspace| workspace.name.clone())
        .unwrap_or_default();
    let tabs = member_tabs(world, context.workspace);
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
                        id: component.id,
                        command: component.argv.clone(),
                        pid: component.state.pid(),
                        cwd: component.cwd.clone(),
                        title: component.published_title.clone(),
                        progress: component
                            .terminal
                            .progress()
                            .map(|progress| (progress.state, progress.percent)),
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
        name,
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
