//! Attachment stream (prompt 3.10): loopback listener, per-viewer admission and projection,
//! runner-side delivery.
//!
//! Ownership: tasks own sockets ([`listener`]); the World owns viewers and decides admission
//! ([`admit`]) and what each viewer is sent ([`projection`]); the runner-owned
//! [`AttachAdapter`] turns `Effect::SendFrame`/`Effect::CloseViewer` into socket writes.
//! Closing is one path: something marks a viewer `Detaching`, the projection sends `Bye` with
//! a reason derived from the World and emits `Effect::CloseViewer`, the adapter drops the
//! writer, the socket shuts down, the reader task sends `Inbound::ViewerGone`, and the
//! lifecycle detaches the viewer. A client that just closes its socket enters the same path at
//! `ViewerGone`.

mod adapter;
mod listener;
mod projection;

use async_channel::Sender;
use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_log::debug;

pub use adapter::AttachAdapter;
pub use listener::{AttachEndpoint, AttachToken, HELLO_TIMEOUT, WRITER_QUEUE, random_hex256};

use crate::layout::{self, LayoutSystems};
use crate::model::{
    Effect, Ids, Inbound, Open, PaneIn, Phase, Process, Retiring, ServerInstance, ViewerId,
    WorkspaceStream,
};
use crate::wire::{self, ByeReason, Hello, ServerFrame, Welcome};

pub struct AttachPlugin {
    /// The runner's channel: reader tasks send `ViewerRequest`/`ViewerGone`, the accept loop
    /// sends `Wake` after handing over a connection.
    pub inbound: Sender<Inbound>,
}

impl Plugin for AttachPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(listener::Inbox::new(self.inbound.clone()))
            .init_resource::<adapter::Registry>()
            .add_systems(Startup, listener::start)
            .add_systems(First, admit.in_set(Phase::Ingest))
            .add_systems(
                PostUpdate,
                projection::project
                    .in_set(Phase::Projection)
                    .after(LayoutSystems::SizeFold)
                    .after(crate::pty::TerminalSystems::Resize),
            );
    }
}

enum Refusal {
    Refused(&'static str),
    Layout(String),
}

/// `First`/`Ingest`: every connection whose `Hello` passed the token check becomes a viewer or
/// is refused. Refusals are written straight to the connection's writer because no viewer
/// exists for an effect to name; admitted viewers get `Welcome` through `Effect::SendFrame`
/// after the adapter learned about them, in the same update as their first scene frame.
fn admit(world: &mut World) {
    let accepted = world.resource::<listener::Inbox>().accepted.clone();
    while let Ok(connection) = accepted.try_recv() {
        match admit_one(world, &connection.hello) {
            Ok((viewer, welcome)) => {
                let registry = world.resource::<adapter::Registry>();
                let registered = adapter::Registered {
                    viewer,
                    writer: connection.writer,
                    recycle: connection.recycle,
                };
                if registry.sender.try_send(registered).is_err() {
                    continue;
                }
                let _ = connection.admit.try_send(viewer);
                world.write_message(Effect::SendFrame {
                    viewer,
                    frame: ServerFrame::Welcome(welcome),
                });
            }
            Err(refusal) => {
                let (reason, message) = match refusal {
                    Refusal::Refused(message) => (ByeReason::Refused, message.to_owned()),
                    Refusal::Layout(message) => (ByeReason::Refused, message),
                };
                debug!("attachment refused: {message}");
                let mut buf = Vec::new();
                if wire::encode(&ServerFrame::Bye { reason, message }, &mut buf).is_ok() {
                    let _ = connection.writer.try_send(buf);
                }
            }
        }
    }
}

fn admit_one(world: &mut World, hello: &Hello) -> Result<(Entity, Welcome), Refusal> {
    let ids = world.resource::<Ids>();
    let workspace = ids
        .workspace(&hello.workspace)
        .ok_or(Refusal::Refused("unknown workspace"))?;
    if world.get::<Open>(workspace).is_none() || world.get::<Retiring>(workspace).is_some() {
        return Err(Refusal::Refused("workspace is not open"));
    }
    let exact = match &hello.exact_target {
        None => None,
        Some(spec) => {
            let pane = ids
                .pane(spec.pane)
                .ok_or(Refusal::Refused("unknown pane"))?;
            if world.get::<PaneIn>(pane).map(|p| p.0) != Some(workspace) {
                return Err(Refusal::Refused("pane is not in the workspace"));
            }
            let process = world.get::<Process>(pane).copied();
            if process.is_some_and(|p| matches!(p, Process::Exited { .. })) {
                return Err(Refusal::Refused("pane has exited"));
            }
            if let Some(pid) = spec.pid
                && process.and_then(Process::pid) != Some(pid)
            {
                return Err(Refusal::Refused("pane pid mismatch"));
            }
            Some(pane)
        }
    };
    let viewer = layout::ops::attach_viewer(world, workspace, hello.viewport, exact)
        .map_err(|error| Refusal::Layout(error.to_string()))?;
    let mut entity = world.entity_mut(viewer);
    if !hello.stream.is_empty() {
        entity.insert(WorkspaceStream(hello.stream.clone()));
    }
    entity.insert(projection::Projected::default());
    let id = *entity
        .get::<ViewerId>()
        .ok_or(Refusal::Layout("viewer has no ViewerId".to_owned()))?;
    Ok((
        viewer,
        Welcome {
            viewer: id,
            instance: world.resource::<ServerInstance>().nonce.clone(),
            workspace: hello.workspace.clone(),
        },
    ))
}
