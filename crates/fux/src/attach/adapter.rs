//! Runner-side effect applier for the attachment stream (prompt 3.6): owns every admitted
//! connection's writer channel and encode buffers, so the World never touches a socket.

use async_channel::{Receiver, Sender, TrySendError};
use bevy_app::App;
use bevy_ecs::error::BevyError;
use bevy_ecs::prelude::*;
use bevy_log::{debug, warn};
use bevy_platform::collections::HashMap;

use crate::model::Effect;
use crate::wire::{self, ByeReason, ServerFrame};

/// An admitted connection, handed from the ingest system to the adapter.
pub(crate) struct Registered {
    pub viewer: Entity,
    pub writer: Sender<Vec<u8>>,
    pub recycle: Receiver<Vec<u8>>,
}

/// Both ends of the registration channel; the plugin inserts it, the adapter clones the
/// receiver out with [`AttachAdapter::from_app`].
#[derive(Resource)]
pub(crate) struct Registry {
    pub sender: Sender<Registered>,
    receiver: Receiver<Registered>,
}

impl Default for Registry {
    fn default() -> Self {
        let (sender, receiver) = async_channel::unbounded();
        Self { sender, receiver }
    }
}

struct Connection {
    writer: Sender<Vec<u8>>,
    recycle: Receiver<Vec<u8>>,
    bye_sent: bool,
}

enum SendError {
    Stalled,
    Closed,
    Encode(serde_json::Error),
}

impl Connection {
    fn send(&mut self, frame: &ServerFrame) -> Result<(), SendError> {
        // Steady state reuses the buffers the writer task hands back; a fresh viewer or a
        // burst deeper than the queue allocates.
        let mut buf = self.recycle.try_recv().unwrap_or_default();
        wire::encode(frame, &mut buf).map_err(SendError::Encode)?;
        if matches!(frame, ServerFrame::Bye { .. }) {
            self.bye_sent = true;
        }
        match self.writer.try_send(buf) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(SendError::Stalled),
            Err(TrySendError::Closed(_)) => Err(SendError::Closed),
        }
    }
}

/// Applies `Effect::SendFrame` and `Effect::CloseViewer`; everything else is not handled.
pub struct AttachAdapter {
    registry: Receiver<Registered>,
    connections: HashMap<Entity, Connection>,
}

impl AttachAdapter {
    /// Builds the adapter for an app that has `AttachPlugin`.
    pub fn from_app(app: &App) -> Result<Self, BevyError> {
        let registry = app
            .world()
            .get_resource::<Registry>()
            .ok_or_else(|| std::io::Error::other("AttachPlugin is not in the app"))?;
        Ok(Self {
            registry: registry.receiver.clone(),
            connections: HashMap::default(),
        })
    }

    pub fn handles(effect: &Effect) -> bool {
        matches!(
            effect,
            Effect::SendFrame { .. } | Effect::CloseViewer { .. }
        )
    }

    /// Admitted connections since the last call.
    pub fn live_count(&mut self) -> usize {
        self.drain_registry();
        self.connections.len()
    }

    /// Returns `false` for effects this adapter does not handle.
    pub fn apply(&mut self, effect: Effect) -> bool {
        self.drain_registry();
        match effect {
            Effect::SendFrame { viewer, frame } => {
                let Some(connection) = self.connections.get_mut(&viewer) else {
                    debug!("frame for unknown viewer {viewer} dropped");
                    return true;
                };
                match connection.send(&frame) {
                    Ok(()) => {}
                    Err(SendError::Stalled) => {
                        // The viewer is not draining its socket; dropping the writer shuts
                        // the socket down and the reader task reports `ViewerGone`.
                        warn!("viewer {viewer} stalled; closing");
                        self.connections.remove(&viewer);
                    }
                    Err(SendError::Closed) => {
                        self.connections.remove(&viewer);
                    }
                    Err(SendError::Encode(error)) => {
                        warn!("frame for viewer {viewer} failed to encode: {error}");
                    }
                }
                true
            }
            Effect::CloseViewer { viewer } => {
                if let Some(mut connection) = self.connections.remove(&viewer)
                    && !connection.bye_sent
                {
                    let _ = connection.send(&ServerFrame::Bye {
                        reason: ByeReason::Detached,
                        message: String::new(),
                    });
                }
                true
            }
            _ => false,
        }
    }

    fn drain_registry(&mut self) {
        while let Ok(registered) = self.registry.try_recv() {
            self.connections.insert(
                registered.viewer,
                Connection {
                    writer: registered.writer,
                    recycle: registered.recycle,
                    bye_sent: false,
                },
            );
        }
    }
}
