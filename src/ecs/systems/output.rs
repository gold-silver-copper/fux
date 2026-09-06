//! Output phase: pane bytes into emulators, host query replies back out, EOF/exit records.

use crate::ecs::components::{Pane, PaneState};
use crate::ecs::messages::{Effect, Inbound};
use crate::ecs::resources::{Clock, Limits};
use crate::ecs::support::{effect, event, pane_entity, pane_workspace};
use crate::proto::control::Event;
use bevy_ecs::prelude::*;

pub fn apply_pane_output(world: &mut World) {
    let now = world.resource::<Clock>().now_ms;
    let interval = world.resource::<Limits>().output_event_interval_ms;
    let inbound: Vec<Inbound> = world
        .resource::<Messages<Inbound>>()
        .iter_current_update_messages()
        .filter(|message| {
            matches!(
                message,
                Inbound::PaneOutput { .. } | Inbound::PaneEof { .. } | Inbound::PaneExited { .. }
            )
        })
        .cloned()
        .collect();
    for message in inbound {
        match message {
            Inbound::PaneOutput { pane, bytes } => {
                let Some(entity) = pane_entity(world, pane) else {
                    continue;
                };
                let Some(mut component) = world.get_mut::<Pane>(entity) else {
                    continue;
                };
                component.terminal.process(&bytes);
                component.dirty = true;
                let replies = component.terminal.take_host_replies();
                let title = component.terminal.title().to_owned();
                let title_changed = title != component.published_title;
                if title_changed {
                    component.published_title = title.clone();
                }
                let publish_output = component
                    .last_output_event_ms
                    .is_none_or(|previous| now.saturating_sub(previous) >= interval);
                if publish_output {
                    component.last_output_event_ms = Some(now);
                }
                let accepts = component.state.accepts_input();
                if !replies.is_empty() && accepts {
                    effect(
                        world,
                        Effect::WriteInput {
                            pane,
                            bytes: replies,
                        },
                    );
                }
                if let Some(workspace) = pane_workspace(world, entity) {
                    if publish_output {
                        event(world, workspace, Event::PaneOutput { id: 0, pane });
                    }
                    if title_changed {
                        event(world, workspace, Event::PaneTitle { id: 0, pane, title });
                    }
                }
            }
            Inbound::PaneEof { pane } => {
                if let Some(entity) = pane_entity(world, pane)
                    && let Some(mut component) = world.get_mut::<Pane>(entity)
                    && let PaneState::Live { pid } = component.state
                {
                    component.state = PaneState::Eof { pid };
                }
            }
            Inbound::PaneExited { pane, code } => {
                // A short-lived process can exit before its spawn completion is applied (the
                // reader thread starts with the process); the status is kept and the completion
                // places an already exited pane, which the lifecycle phase then closes.
                if let Some(entity) = pane_entity(world, pane)
                    && let Some(mut component) = world.get_mut::<Pane>(entity)
                {
                    component.state = PaneState::Exited { code };
                    component.dirty = true;
                }
            }
            _ => {}
        }
    }
}
