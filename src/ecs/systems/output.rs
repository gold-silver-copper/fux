//! Output phase: pane bytes into emulators, host query replies back out, EOF/exit records.

use crate::ecs::components::{Pane, PaneState, Tab, Workspace};
use crate::ecs::messages::{Effect, Inbound};
use crate::ecs::support::{Effects, Step};
use crate::proto::control::Event;
use bevy_ecs::prelude::*;

/// Applies this step's pane events in arrival order: bytes, then EOF, then the exit status.
pub fn apply_pane_output(
    mut inbound: MessageReader<Inbound>,
    step: Step,
    mut panes: Query<&mut Pane>,
    tabs: Query<&Tab>,
    workspaces: Query<&Workspace>,
    mut effects: Effects,
) {
    let now = step.clock.now_ms;
    let interval = step.limits.output_event_interval_ms;
    let ids = &step.ids;
    for message in inbound.read() {
        match message {
            Inbound::PaneOutput { pane, bytes } => {
                let Some(mut component) = ids.pane(*pane).and_then(|e| panes.get_mut(e).ok())
                else {
                    continue;
                };
                component.terminal.process(bytes);
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
                if !replies.is_empty() && component.state.accepts_input() {
                    effects.emit(Effect::WriteInput {
                        pane: *pane,
                        bytes: replies,
                    });
                }
                let workspace = tabs
                    .get(component.tab)
                    .and_then(|tab| workspaces.get(tab.workspace))
                    .map(|workspace| workspace.name.clone());
                if let Ok(workspace) = workspace {
                    if publish_output {
                        effects.event(&workspace, Event::PaneOutput { id: 0, pane: *pane });
                    }
                    if title_changed {
                        effects.event(
                            &workspace,
                            Event::PaneTitle {
                                id: 0,
                                pane: *pane,
                                title,
                            },
                        );
                    }
                }
            }
            Inbound::PaneEof { pane } => {
                if let Some(mut component) = ids.pane(*pane).and_then(|e| panes.get_mut(e).ok())
                    && let PaneState::Live { pid } = component.state
                {
                    component.state = PaneState::Eof { pid };
                }
            }
            Inbound::PaneExited { pane, code } => {
                // A short-lived process can exit before its spawn completion is applied (the
                // reader thread starts with the process); the status is kept and the completion
                // places an already exited pane, which the lifecycle phase then closes.
                if let Some(mut component) = ids.pane(*pane).and_then(|e| panes.get_mut(e).ok()) {
                    component.state = PaneState::Exited { code: *code };
                    component.dirty = true;
                }
            }
            _ => {}
        }
    }
}
