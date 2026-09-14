//! Ordered, bounded external effects. Local input parsing never waits on this queue.
use crate::{
    proto::{attach::MouseEvent, control::Request},
    view::Frame,
};
use std::collections::VecDeque;

pub enum Effect {
    Input(Vec<u8>),
    Control(Request),
    Mouse {
        event: MouseEvent,
        generation: u64,
    },
    Manager {
        request: crate::daemon::ManagerRequest,
        epoch: u64,
    },
    Detach,
}

/// The attachment identity a modal interaction was opened against; any change dismisses it.
#[derive(Clone, PartialEq, Eq)]
pub struct Identity {
    pub instance: String,
    pub workspace: String,
    pub stream: u64,
    pub viewer: crate::ids::ViewerId,
}
impl Identity {
    pub fn of(frame: &Frame) -> Self {
        Self {
            instance: frame.server_instance.clone(),
            workspace: frame.workspace.clone(),
            stream: frame.workspace_stream,
            viewer: frame.viewer,
        }
    }
    pub fn matches(&self, frame: &Frame) -> bool {
        frame.server_instance == self.instance
            && frame.workspace == self.workspace
            && frame.workspace_stream == self.stream
            && frame.viewer == self.viewer
    }
}
pub fn navigates_workspace(request: &Request) -> bool {
    matches!(
        request,
        Request::Workspace {
            action: crate::proto::control::WorkspaceAction::Select { .. }
                | crate::proto::control::WorkspaceAction::New { .. },
            ..
        }
    )
}
struct Entry {
    effect: Effect,
    bytes: usize,
    identity: Option<Identity>,
    focused: Option<Option<crate::ids::PaneId>>,
}
#[derive(Default)]
pub struct Queue {
    entries: VecDeque<Entry>,
    bytes: usize,
}
impl Queue {
    pub fn navigates_workspace(&self) -> bool {
        self.entries.iter().any(|entry| {
            matches!(&entry.effect,
            Effect::Control(request) if navigates_workspace(request))
        })
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn manager_pending(&self) -> bool {
        self.entries.iter().any(|entry| matches!(&entry.effect,
            Effect::Manager { request, .. } if !matches!(request, crate::daemon::ManagerRequest::Catalog)))
    }
    pub fn push(&mut self, effect: Effect, fixed: Option<&Frame>) -> anyhow::Result<()> {
        let bytes = match &effect {
            Effect::Input(bytes) => bytes.len(),
            Effect::Control(request) => serde_json::to_vec(request)?.len(),
            Effect::Manager { request, .. } => serde_json::to_vec(request)?.len(),
            _ => 64,
        };
        anyhow::ensure!(
            self.bytes.saturating_add(bytes) <= 64 * 1024 && self.entries.len() < 256,
            "external effects buffered beyond limit while waiting for the server"
        );
        self.bytes += bytes;
        tracing::debug!(target: "fux::diagnostics", pid = std::process::id(), event = "effect_queued", bytes = self.bytes, entries = self.entries.len() + 1,
            request = ?match &effect { Effect::Control(request) => Some(request.id()), _ => None });
        let focused = if matches!(effect, Effect::Input(_)) {
            fixed.map(|frame| frame.focused)
        } else {
            None
        };
        self.entries.push_back(Entry {
            effect,
            bytes,
            identity: fixed.map(Identity::of),
            focused,
        });
        Ok(())
    }
    /// Fixed-context effects must not silently migrate to a replacement workspace.
    pub fn pop(&mut self, frame: &Frame) -> Option<Result<Effect, &'static str>> {
        let entry = self.entries.pop_front()?;
        self.bytes -= entry.bytes;
        Some(
            if entry
                .identity
                .as_ref()
                .is_some_and(|identity| *identity != Identity::of(frame))
                || entry.focused.is_some_and(|focused| {
                    focused != frame.focused
                        || focused.is_some_and(|pane| {
                            frame.pane(pane).is_none_or(|view| view.exit.is_some())
                        })
                })
            {
                tracing::debug!(target: "fux::diagnostics", pid = std::process::id(), event = "effect_target_invalidated", viewer = frame.viewer.0, stream = frame.workspace_stream);
                Err("Queued input or operation discarded: its target changed")
            } else {
                match (entry.effect, entry.identity, entry.focused) {
                    // The manager uses another connection: its reply can arrive before
                    // the attachment's new frame. Address the original pane on the
                    // server as well as checking the local snapshot before sending.
                    (Effect::Input(bytes), Some(identity), Some(Some(pane))) => {
                        Ok(Effect::Control(Request::SendKeys {
                            id: 0,
                            instance: Some(identity.instance),
                            pane,
                            keys: bytes.iter().map(|byte| format!("\\x{byte:02x}")).collect(),
                            notation: crate::proto::control::KeyNotation::Escapes,
                        }))
                    }
                    (Effect::Input(_), Some(_), Some(None)) => {
                        Err("No live pane to receive queued input")
                    }
                    (effect, _, _) => Ok(effect),
                }
            },
        )
    }
    pub fn input(&mut self, bytes: &mut Vec<u8>, fixed: Option<&Frame>) -> anyhow::Result<()> {
        for chunk in bytes.chunks(crate::proto::attach::MAX_INPUT_CHUNK) {
            self.push(Effect::Input(chunk.to_vec()), fixed)?;
        }
        bytes.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manager_delayed_input_is_byte_exact_and_server_targeted() {
        let mut frame = Frame::default();
        let pane = crate::ids::PaneId(7);
        frame.focused = Some(pane);
        frame.server_instance = "fixture-instance".into();
        frame.panes.insert(pane, crate::view::PaneView::default());
        let original = vec![0, 27, 255, b'\\', 0xc3, 0xa9];
        let mut bytes = original.clone();
        let mut queue = Queue::default();
        queue
            .input(&mut bytes, Some(&frame))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(bytes.is_empty());
        let Some(Ok(Effect::Control(Request::SendKeys {
            instance,
            pane: target,
            keys,
            notation,
            ..
        }))) = queue.pop(&frame)
        else {
            panic!("expected explicitly targeted input");
        };
        assert_eq!(target, pane);
        assert_eq!(instance.as_deref(), Some("fixture-instance"));
        assert_eq!(
            crate::proto::control::decode_keys(&keys, notation).unwrap_or_else(|e| panic!("{e}")),
            original
        );
        queue
            .input(&mut original.clone(), Some(&frame))
            .unwrap_or_else(|e| panic!("{e}"));
        frame.focused = None;
        assert!(matches!(queue.pop(&frame), Some(Err(_))));
    }

    #[test]
    fn queue_is_bounded_ordered_and_rejects_a_replacement_workspace() {
        let mut queue = Queue::default();
        let mut frame = Frame::default();
        queue
            .push(Effect::Input(vec![1; 65536]), Some(&frame))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(queue.push(Effect::Detach, None).is_err());
        frame.workspace_stream += 1;
        assert!(matches!(queue.pop(&frame), Some(Err(_))));
        assert!(queue.is_empty());
        queue
            .push(Effect::Input(vec![2]), None)
            .unwrap_or_else(|e| panic!("{e}"));
        queue
            .push(Effect::Detach, None)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(queue.pop(&frame), Some(Ok(Effect::Input(bytes))) if bytes == [2]));
        assert!(matches!(queue.pop(&frame), Some(Ok(Effect::Detach))));
    }
}
