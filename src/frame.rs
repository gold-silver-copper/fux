//! Per-viewer presentation contexts, size negotiation and frame painting.
#[cfg(test)]
mod tests;
use crate::{
    actions,
    assets::{BindingAction, Settings},
    chrome::{self, at, fit},
    model::*,
    presentation::Presentation,
    protocol::{Direction, Frame},
    server::{invalidate_layouts, scene},
    terminal::Terminal,
};
use base64::Engine;
use bevy_ecs::{entity::EntityHashMap, prelude::*};
use bevy_remote::{BrpError, BrpResult};
use serde_json::{Value, json};
use std::{
    fmt::Write,
    time::{Duration, Instant},
};

pub(crate) fn sync_view(world: &mut World, id: Entity) -> Result<(), String> {
    // Controls can arrive before the next Update; run the same native change-
    // tracking system at this synchronous input/presentation boundary too.
    world
        .run_system_cached(invalidate_layouts)
        .map_err(|e| e.to_string())?;
    let root = viewing(world, id).ok_or(DETACHED)?;
    let (revision, scene) = scene(world, root)?;
    let v = world.get::<Viewer>(id).ok_or(DETACHED)?.clone();
    if world.get::<Presentation>(id).is_none() {
        let registry = world.resource::<AppTypeRegistry>().clone();
        world.entity_mut(id).insert(Presentation::new(registry));
    }
    let state = (on_tab(world, id), focused(world, id));
    // A raw hierarchy edit can take the zoomed pane out of the workspace
    // without touching the viewer's focus. Zoom then has nothing to show:
    // paint the tab instead of failing the frame, which would end the
    // viewer's session.
    let v = if v.zoom
        && !state.1.is_some_and(|leaf| {
            world.get::<PaneView>(leaf).is_some() && scene.entities.iter().any(|e| e.entity == leaf)
        }) {
        world.get_mut::<Viewer>(id).ok_or(DETACHED)?.zoom = false;
        world.get::<Viewer>(id).ok_or(DETACHED)?.clone()
    } else {
        v
    };
    let mut context = world
        .get_mut::<Presentation>(id)
        .ok_or("missing presentation")?;
    context.sync(&scene, revision, root, &v, state)?;
    if v.rows > 1 && v.cols > 0 && !context.rects().iter().any(|r| Some(r.leaf) == state.1) {
        // Zoom and hidden tabs can leave the focused pane unpainted; follow
        // the projection rather than paint input into an invisible pane.
        let first = context.rects().first().map(|r| r.leaf);
        let mut entity = world.get_entity_mut(id).map_err(|_| DETACHED)?;
        match first {
            Some(leaf) => {
                entity.insert(Focused(leaf));
            }
            None => {
                entity.remove::<Focused>();
            }
        }
        let state = (on_tab(world, id), focused(world, id));
        let v = world.get::<Viewer>(id).ok_or(DETACHED)?.clone();
        world
            .get_mut::<Presentation>(id)
            .ok_or("missing presentation")?
            .sync(&scene, revision, root, &v, state)?;
    }
    Ok(())
}
fn size_terminals(world: &mut World) {
    let mut sizes = EntityHashMap::<(u16, u16)>::default();
    for context in world
        .query_filtered::<&Presentation, IsViewer>()
        .iter(world)
    {
        for rect in context.rects() {
            let rows = rect.height();
            let cols = rect.width();
            sizes
                .entry(rect.pane)
                .and_modify(|s| {
                    s.0 = s.0.min(rows);
                    s.1 = s.1.min(cols)
                })
                .or_insert((rows, cols));
        }
    }
    for (pane, (rows, cols)) in sizes {
        if let Some(mut terminal) = world.get_mut::<Terminal>(pane) {
            let _ = terminal.resize(rows, cols);
        }
    }
}
pub(crate) fn attach(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    let params = params.unwrap_or_default();
    let requested = params.get("workspace").and_then(Value::as_str);
    // Closing the last workspace detaches its viewers but must not leave a
    // server nobody can attach to again: recreate the initial workspace.
    if crate::navigation::workspaces(world).is_empty() {
        crate::server::initialize(world);
    }
    let root = crate::navigation::workspaces(world)
        .into_iter()
        .find(|e| {
            requested
                .is_none_or(|wanted| world.get::<Name>(*e).is_some_and(|n| n.as_str() == wanted))
        })
        .ok_or_else(|| BrpError::internal("workspace not found"))?;
    let rows = params
        .get("rows")
        .and_then(Value::as_u64)
        .unwrap_or(24)
        .min(4096) as u16;
    let cols = params
        .get("cols")
        .and_then(Value::as_u64)
        .unwrap_or(80)
        .min(4096) as u16;
    let id = world
        .spawn((
            Viewer {
                rows,
                cols,
                zoom: false,
                scrollback: 0,
                notice: None,
            },
            Viewing(root),
        ))
        .id();
    crate::navigation::repair(world);
    // A workspace a raw hierarchy edit left unprojectable must not refuse
    // every new attach: the viewer's frames name the failure instead.
    let _ = sync_view(world, id);
    Ok(json!({"viewer":id.to_bits()}))
}
fn request_viewer(params: Option<Value>) -> Result<Entity, BrpError> {
    let id = params
        .and_then(|p| p.get("viewer").and_then(Value::as_u64))
        .ok_or_else(|| BrpError::internal("viewer entity required"))?;
    // Any u64 arrives here; `from_bits` panics on one that no entity can
    // have (0, for one), which would take down every session.
    Entity::try_from_bits(id).ok_or_else(|| BrpError {
        code: bevy_remote::error_codes::INVALID_PARAMS,
        message: format!("{id} is not an entity id"),
        data: None,
    })
}
pub(crate) fn frame(In(params): In<Option<Value>>, world: &mut World) -> BrpResult {
    make_frame(world, request_viewer(params)?)
        .map(|f| json!(f))
        .map_err(BrpError::internal)
}
pub(crate) fn frame_watch(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult<Option<Value>> {
    let id = request_viewer(params)?;
    let now = Instant::now();
    let deferred = world.get_mut::<Presentation>(id).and_then(|mut view| {
        if now >= view.next_paint {
            view.paint_wake_pending = false;
            None
        } else {
            let needs_wake = !view.paint_wake_pending;
            view.paint_wake_pending = true;
            Some((view.next_paint, needs_wake))
        }
    });
    if let Some((deadline, needs_wake)) = deferred {
        if needs_wake {
            // Coalesce hot output before painting, without a periodic idle tick.
            let wake = world.resource::<Wake>().clone();
            bevy_tasks::IoTaskPool::get()
                .spawn(async move {
                    async_io::Timer::at(deadline).await;
                    wake.notify();
                })
                .detach();
        }
        return Ok(None);
    }
    let has_effect = world
        .get::<Presentation>(id)
        .is_some_and(|view| !view.clipboard.is_empty());
    let frame = make_frame(world, id).map_err(BrpError::internal)?;
    if frame.detach {
        return Ok(Some(json!(frame)));
    }
    let Some(mut view) = world.get_mut::<Presentation>(id) else {
        return Ok(None);
    };
    if !has_effect && view.last == frame.paint {
        return Ok(None);
    }
    view.last.clone_from(&frame.paint);
    view.next_paint = Instant::now() + Duration::from_millis(16);
    Ok(Some(json!(frame)))
}
fn make_frame(world: &mut World, id: Entity) -> Result<Frame, String> {
    if world.get::<Viewer>(id).is_none() {
        return Ok(Frame {
            paint: String::new(),
            detach: true,
        });
    }
    // Size negotiation must never count another viewer's stale, now-hidden
    // tab projection merely because that viewer has not requested a frame.
    let viewers: Vec<_> = world
        .query_filtered::<Entity, IsViewer>()
        .iter(world)
        .collect();
    // One viewer's unextractable workspace, which a raw hierarchy edit can
    // produce, must not fail every other viewer's frame; and the viewer
    // itself gets a bar that says what is wrong rather than a failed
    // request, which would end its session.
    let mut own = Ok(());
    for viewer in viewers {
        let synced = sync_view(world, viewer);
        if viewer == id {
            own = synced;
        }
    }
    size_terminals(world);
    if let Err(error) = own {
        return Ok(degraded(world, id, &error));
    }
    if let Some(selection) = world.get::<crate::selection::Selection>(id) {
        let visible = rect(world, id, selection.leaf).map_or((0, 0), |r| (r.height(), r.width()));
        crate::selection::refresh_visible(world, id, visible);
    }
    paint(world, id)
}

/// A frame with no content and the failure in the bar, for a viewer whose
/// layout cannot be projected.
fn degraded(world: &World, id: Entity, error: &str) -> Frame {
    let mut out = String::from("\x1b[?2026h\x1b[?7l\x1b[?25l\x1b[0m\x1b[H\x1b[2J");
    if let Some(v) = world.get::<Viewer>(id)
        && v.rows > 0
        && v.cols > 0
    {
        at(
            &mut out,
            0,
            v.rows - 1,
            format_args!("{}{}", chrome::BAR, " ".repeat(usize::from(v.cols))),
        );
        at(
            &mut out,
            0,
            v.rows - 1,
            format_args!("{}\x1b[31m{}", chrome::BAR, fit(error, v.cols, false)),
        );
    }
    out.push_str("\x1b[0m\x1b[?2026l");
    Frame {
        paint: out,
        detach: false,
    }
}

pub(crate) fn rect(
    world: &World,
    viewer: Entity,
    leaf: Entity,
) -> Option<crate::protocol::PaneRect> {
    world
        .get::<Presentation>(viewer)?
        .rects()
        .iter()
        .find(|r| r.leaf == leaf)
        .copied()
}

pub(crate) fn content_size(world: &World, viewer: Entity, leaf: Entity) -> Option<(u16, u16)> {
    rect(world, viewer, leaf).map(|r| (r.height(), r.width()))
}

pub(crate) fn visible_leaf(world: &World, viewer: Entity, leaf: Entity) -> bool {
    world
        .get::<Presentation>(viewer)
        .is_none_or(|p| p.rects().iter().any(|r| r.leaf == leaf))
}

pub(crate) fn neighbor(
    world: &World,
    viewer: Entity,
    leaf: Entity,
    direction: Direction,
) -> Option<Entity> {
    world.get::<Presentation>(viewer)?.neighbor(leaf, direction)
}

pub(crate) fn directional_neighbor(
    rects: &[crate::protocol::PaneRect],
    leaf: Entity,
    direction: Direction,
) -> Option<Entity> {
    let here = rects.iter().find(|r| r.leaf == leaf)?;
    // Doubled centres (min + max) keep odd sizes exact; `center()` would truncate.
    let centre = |r: &crate::protocol::PaneRect| (r.rect.min + r.rect.max).as_ivec2();
    let here = centre(here);
    rects
        .iter()
        .filter_map(|r| {
            let delta = centre(r) - here;
            let (dx, dy) = (delta.x, delta.y);
            let (forward, cross) = match direction {
                Direction::Left => (-dx, dy.abs()),
                Direction::Right => (dx, dy.abs()),
                Direction::Up => (-dy, dx.abs()),
                Direction::Down => (dy, dx.abs()),
            };
            (forward > 0).then_some(((cross, forward, r.leaf.to_bits()), r.leaf))
        })
        .min_by_key(|(score, _)| *score)
        .map(|(_, leaf)| leaf)
}

fn name(world: &World, entity: Entity) -> String {
    world
        .get::<Name>(entity)
        .map_or_else(|| entity.to_bits().to_string(), |n| n.as_str().to_owned())
}

pub(crate) fn clipboard(world: &mut World, id: Entity, text: String) -> Result<(), String> {
    crate::selection::validate_clipboard(world.resource::<Settings>(), &text)?;
    let mut view = world
        .get_mut::<Presentation>(id)
        .ok_or("presentation not initialized")?;
    if view.clipboard.len() == 16 {
        return Err("clipboard delivery queue is full".into());
    }
    view.clipboard.push(text);
    notify(world, id, Notice::info("selection copied via OSC52"));
    Ok(())
}

fn process_status(world: &World, pane: Entity) -> String {
    match world.get::<ProcessState>(pane).map(|s| &s.status) {
        Some(Status::Exited { code }) => format!(" [exit:{code}]"),
        Some(Status::Failed { error })
        | Some(Status::Running {
            error: Some(error), ..
        }) => format!(" [{error}]"),
        _ => String::new(),
    }
}

fn paint(world: &mut World, id: Entity) -> Result<Frame, String> {
    let v = world.get::<Viewer>(id).ok_or("viewer no longer attached")?;
    let focus = focused(world, id);
    let scrollback = v.scrollback;
    let modal = crate::interaction::modal(world, id);
    let rects = world
        .get::<Presentation>(id)
        .ok_or("missing presentation")?
        .rects()
        .to_vec();
    let mut out = String::from("\x1b[?2026h\x1b[?7l\x1b[?25l\x1b[0m\x1b[H\x1b[2J");
    let focused = focus
        .and_then(|leaf| world.get::<PaneView>(leaf))
        .map_or_else(String::new, |view| {
            format!(
                "{}: {}{}",
                view.pane.to_bits(),
                name(world, view.pane),
                process_status(world, view.pane)
            )
        });
    let mut cursor = None;
    for rect in &rects {
        let selected = focus == Some(rect.leaf);
        let status = process_status(world, rect.pane);
        let exited = !status.is_empty();
        match world.get_mut::<Terminal>(rect.pane) {
            Some(mut terminal) => {
                let (lines, screen) = terminal.snapshot(
                    if selected { scrollback } else { 0 },
                    rect.height(),
                    rect.width(),
                );
                for (row, line) in lines.iter().take(usize::from(rect.height())).enumerate() {
                    // Snapshot rows already reset style at both ends.
                    at(&mut out, rect.x(), rect.y() + row as u16, line.as_ref());
                }
                let (row, col) = screen.cursor_position();
                if selected
                    && !screen.hide_cursor()
                    && scrollback == 0
                    && !exited
                    && row < rect.height()
                    && col < rect.width()
                {
                    cursor = Some((rect.x() + col, rect.y() + row));
                }
            }
            None => at(
                &mut out,
                rect.x(),
                rect.y(),
                fit("terminal not found", rect.width(), false),
            ),
        }
        if !selected && exited {
            let label = fit(&status, rect.width(), false);
            at(
                &mut out,
                rect.x() + rect.width() - chrome::width(&label),
                rect.y() + rect.height() - 1,
                format_args!("\x1b[0;2;7m{label}\x1b[0m"),
            );
        }
    }
    if let Some(selection) = world.get::<crate::selection::Selection>(id)
        && let Some(rect) = rects.iter().find(|r| r.leaf == selection.leaf)
    {
        crate::selection::paint(&mut out, selection, rect);
    }
    world
        .get::<Presentation>(id)
        .ok_or("missing presentation")?
        .paint_separators(&mut out, focus);
    let v = world.get::<Viewer>(id).ok_or("viewer no longer attached")?;
    let workspace = viewing(world, id).ok_or(DETACHED)?;
    let tabs: Vec<_> = crate::navigation::tabs(world, workspace)
        .into_iter()
        .map(|tab| (tab, name(world, tab)))
        .collect();
    let hits = chrome::tab_bar(
        &mut out,
        v,
        (workspace, &name(world, workspace)),
        (on_tab(world, id), &tabs),
        &focused,
    );
    world
        .get_mut::<Presentation>(id)
        .ok_or("missing presentation")?
        .chrome(hits);
    let v = world.get::<Viewer>(id).ok_or(DETACHED)?;
    if let Some(overlay) = world.get::<crate::interaction::Overlay>(id) {
        chrome::surface(
            &mut out,
            v,
            &crate::interaction::lines(world, overlay, v.rows),
        )
    } else if let Some(prefix) = world.get::<crate::interaction::Prefix>(id) {
        chrome::panel_context(
            &mut out,
            v,
            world.resource::<Settings>(),
            prefix.scroll,
            |binding| {
                let Some(target) = actions::Target::of(world, id) else {
                    return true;
                };
                match binding {
                    BindingAction::Known(action) => {
                        actions::unavailable(world, target, *action).is_some()
                    }
                    BindingAction::Custom(_) => !target.valid(world),
                }
            },
        )
    } else {
        None
    };
    let disabled =
        world.resource::<Settings>().clipboard == crate::assets::ClipboardPolicy::Disabled;
    let mut view = world
        .get_mut::<Presentation>(id)
        .ok_or("missing presentation")?;
    if disabled {
        view.clipboard.clear();
    }
    for text in view.clipboard.drain(..) {
        let _ = write!(
            out,
            "\x1b]52;c;{}\x07",
            base64::engine::general_purpose::STANDARD.encode(text)
        );
    }
    if !modal && let Some((x, y)) = cursor {
        at(&mut out, x, y, "\x1b[?25h");
    }
    out.push_str("\x1b[0m\x1b[?2026l");
    Ok(Frame {
        paint: out,
        detach: false,
    })
}
