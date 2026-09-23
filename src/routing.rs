//! The two routed observers: `Control` runs a command for a viewer, and
//! `UserInput` passes through paste, overlays, selection and bindings before
//! reaching the focused pane.
use crate::{
    assets::Settings,
    control::{Command, Control, Order, Scope, Subject, UserInput},
    execute::execute,
    frame::{self, sync_view},
    interaction::Prefix,
    model::*,
    presentation::Presentation,
    protocol::{Input, MouseAction, MouseButton, Token},
    terminal::Terminal,
};
use bevy_ecs::prelude::*;

pub(crate) fn route_control(event: On<Control>, mut commands: Commands) {
    let (viewer, command) = (event.event_target(), event.command.clone());
    commands.queue(move |world: &mut World| {
        if sync_view(world, viewer).is_err() {
            return;
        }
        if let Err(error) = execute(world, viewer, command) {
            notify(world, viewer, Notice::error(error));
        }
        world.resource::<Wake>().notify();
    });
}

pub(crate) fn route_input(event: On<UserInput>, mut commands: Commands) {
    let event = event.event().clone();
    commands.queue(move |world: &mut World| {
        if sync_view(world, event.viewer).is_err() {
            return;
        }
        if crate::paste::input(world, event.viewer, &event.input) {
            return;
        }
        if crate::interaction::input(world, event.viewer, &event.input) {
            return;
        }
        if crate::selection::input(world, event.viewer, &event.input) {
            return;
        }
        if crate::interaction::command_input(world, event.viewer, &event.input) {
            return;
        }
        if let Input::Mouse { action, x, y, .. } = &event.input
            && matches!(action, MouseAction::Move | MouseAction::Release)
            && let Some(selection) = world.get::<crate::selection::Selection>(event.viewer)
            && selection.dragging
        {
            let leaf = selection.leaf;
            let rect = frame::rect(world, event.viewer, leaf);
            if let Some(rect) = rect {
                let point = rect.local(*x, *y);
                let _ = crate::selection::mouse(world, event.viewer, leaf, *action, point);
            }
            return;
        }
        if world.get::<Viewer>(event.viewer).is_none() {
            return;
        }
        if let Some(token) = event.input.token()
            && world.get::<Prefix>(event.viewer).is_some()
        {
            let settings = world.resource::<Settings>();
            if token != settings.prefix
                && let Some(binding) = settings.bindings.iter().find(|b| b.key == token)
            {
                let binding = binding.action.clone();
                if let Some(target) = crate::actions::Target::of(world, event.viewer) {
                    crate::interaction::dispatch_binding(world, event.viewer, target, &binding);
                }
                return;
            }
        }
        if world.get::<Viewer>(event.viewer).is_none() {
            return;
        }
        if world.get::<Prefix>(event.viewer).is_none()
            && let Input::Mouse {
                action: MouseAction::Press,
                button,
                x,
                y,
                modifiers,
            } = &event.input
        {
            let shift = &modifiers.shift;
            let hit = world
                .get_mut::<Presentation>(event.viewer)
                .and_then(|mut view| view.pointer(*x, *y, false));
            if let Some(hit) = hit {
                let command = if world.get::<Tab>(hit).is_some() {
                    Some(if *button == MouseButton::Right {
                        Command::Menu {
                            subject: Subject::Tab(hit),
                        }
                    } else {
                        Command::Select {
                            scope: Scope::Tab,
                            entity: hit,
                        }
                    })
                } else if world.get::<Workspace>(hit).is_some() {
                    Some(Command::Menu {
                        subject: Subject::Workspace(hit),
                    })
                } else if *button == MouseButton::Right
                    && world.get::<PaneView>(hit).is_some_and(|p| {
                        *shift
                            || world
                                .get::<crate::selection::Selection>(event.viewer)
                                .is_some()
                            || world.get::<Terminal>(p.pane).is_none_or(|t| {
                                t.screen().mouse_protocol_mode() == fux_vt::MouseProtocolMode::None
                            })
                    })
                {
                    Some(Command::Menu {
                        subject: Subject::Pane(hit),
                    })
                } else {
                    None
                };
                if let Some(command) = command {
                    if let Err(error) = execute(world, event.viewer, command) {
                        notify(world, event.viewer, Notice::error(error));
                    }
                    return;
                }
            }
        }
        if let Input::Mouse {
            action: MouseAction::Press,
            button: MouseButton::Left,
            x,
            y,
            modifiers,
        } = &event.input
            && world.get::<Prefix>(event.viewer).is_none()
        {
            let shift = &modifiers.shift;
            let hit = world
                .get_mut::<Presentation>(event.viewer)
                .and_then(|mut view| view.pointer(*x, *y, false));
            if let Some(hit) = hit
                && let Some(pane) = world.get::<PaneView>(hit).map(|p| p.pane)
                && (*shift
                    || world
                        .get::<crate::selection::Selection>(event.viewer)
                        .is_some()
                    || world.get::<Terminal>(pane).is_some_and(|t| {
                        t.screen().mouse_protocol_mode() == fux_vt::MouseProtocolMode::None
                    }))
            {
                let rect = frame::rect(world, event.viewer, hit);
                if let Some(rect) = rect {
                    if focused(world, event.viewer) != Some(hit)
                        && let Some(mut v) = world.get_mut::<Viewer>(event.viewer)
                    {
                        v.scrollback = 0;
                    }
                    if let Ok(mut viewer) = world.get_entity_mut(event.viewer) {
                        viewer.insert(Focused(hit));
                    }
                    if let Err(error) = crate::selection::mouse(
                        world,
                        event.viewer,
                        hit,
                        MouseAction::Press,
                        rect.local(*x, *y),
                    ) {
                        notify(world, event.viewer, Notice::error(error));
                    }
                    return;
                }
            }
        }
        if matches!(event.input, Input::Mouse { .. })
            && world
                .get::<crate::selection::Selection>(event.viewer)
                .is_some()
        {
            return;
        }
        if let Input::Mouse { y, .. } = &event.input
            && world
                .get::<Viewer>(event.viewer)
                .is_some_and(|v| *y >= v.rows.saturating_sub(1))
        {
            return;
        }
        if let Err(error) = terminal_input(world, event.viewer, &event.input) {
            notify(world, event.viewer, Notice::error(error));
        }
        world.resource::<Wake>().notify();
    });
}

/// Ordinary input for the focused pane: keys and pastes become PTY bytes,
/// mouse events are picked against the painted layout, and the prefix key
/// opens or literally forwards itself.
fn terminal_input(world: &mut World, id: Entity, input: &Input) -> Result<(), String> {
    let prefix = world.get::<Prefix>(id).is_some();
    let focus = focused(world, id);
    let pane_of = |world: &World| {
        focus
            .and_then(|leaf| world.get::<PaneView>(leaf))
            .map(|view| view.pane)
            .ok_or("no focused pane")
    };
    match input {
        Input::PasteBegin => {}
        Input::Resize { rows, cols } => {
            let mut v = world.get_mut::<Viewer>(id).ok_or(DETACHED)?;
            v.rows = (*rows).min(4096);
            v.cols = (*cols).min(4096);
        }
        Input::Key { key, modifiers } => {
            let token = input.token().unwrap_or_else(|| Token::from(""));
            let settings = world.resource::<Settings>();
            // Reserved keys and bound shortcuts were consumed upstream; what
            // reaches here in the column is an unbound key or the literal prefix.
            if prefix {
                if token != settings.prefix {
                    notify(
                        world,
                        id,
                        Notice::info(format!("unbound prefix key {token}")),
                    );
                    return Ok(());
                }
                crate::interaction::close_prefix(world, id);
            } else if token == settings.prefix {
                world.entity_mut(id).insert(Prefix::default());
                notify(world, id, None);
                return Ok(());
            }
            let pane = pane_of(world)?;
            let terminal = world.get::<Terminal>(pane).ok_or("terminal not found")?;
            let application = terminal.screen().application_cursor();
            terminal.input(&crate::encode::key_bytes(*key, *modifiers, application))?;
            let mut v = world.get_mut::<Viewer>(id).ok_or(DETACHED)?;
            v.scrollback = 0;
            v.notice = None;
        }
        Input::Paste { text } => {
            if prefix {
                return Ok(());
            }
            let pane = pane_of(world)?;
            let terminal = world.get::<Terminal>(pane).ok_or("terminal not found")?;
            if terminal.screen().bracketed_paste() {
                terminal.input(&crate::paste::bracketed(text))?;
            } else {
                terminal.input(text.as_bytes())?;
            }
            let mut v = world.get_mut::<Viewer>(id).ok_or(DETACHED)?;
            v.scrollback = 0;
            v.notice = None;
        }
        Input::Mouse {
            action,
            button,
            x,
            y,
            modifiers,
        } => {
            if prefix {
                return Ok(());
            }
            // Pick against the last painted native layout, not a second
            // rectangle hit-test implementation.
            let hit = {
                let mut context = world
                    .get_mut::<Presentation>(id)
                    .ok_or("presentation not initialized")?;
                let hit = context.pointer(*x, *y, *action == MouseAction::Press);
                hit.map(|hit| {
                    context
                        .rects()
                        .iter()
                        .find(|r| r.leaf == hit)
                        .copied()
                        .ok_or("picked pane has no rectangle")
                })
            };
            let Some(rect) = hit.transpose()? else {
                return Ok(());
            };
            let hit = rect.leaf;
            if *action == MouseAction::Press {
                world.entity_mut(id).insert(Focused(hit));
                world.get_mut::<Viewer>(id).ok_or(DETACHED)?.scrollback = 0;
            }
            let terminal = world
                .get::<Terminal>(rect.pane)
                .ok_or("terminal not found")?;
            let screen = terminal.screen();
            let mode = screen.mouse_protocol_mode();
            use fux_vt::MouseProtocolMode as MouseMode;
            if mode == MouseMode::None || modifiers.shift {
                if matches!(action, MouseAction::ScrollUp | MouseAction::ScrollDown) {
                    let order = if *action == MouseAction::ScrollUp {
                        Order::Previous
                    } else {
                        Order::Next
                    };
                    if focus != Some(hit) {
                        world.entity_mut(id).insert(Focused(hit));
                        world.get_mut::<Viewer>(id).ok_or(DETACHED)?.scrollback = 0;
                    }
                    execute(world, id, Command::Scroll { order })?;
                }
            } else if rect.covers(*x, *y)
                && let Some(bytes) = crate::encode::mouse_bytes(
                    screen,
                    *action,
                    *button,
                    (x - rect.x() + 1, y - rect.y() + 1),
                    *modifiers,
                )
            {
                terminal.input(&bytes)?;
            }
        }
    }
    Ok(())
}
