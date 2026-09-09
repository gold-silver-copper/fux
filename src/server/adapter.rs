//! Applies ECS effects to the operating system and turns operating-system activity into typed
//! inbound events. Owns every OS handle (pane processes, viewer outboxes, control replies,
//! workspace sockets) keyed by public ids; the World never sees them.

use crate::daemon::{
    DaemonPaths, Descriptor, ManagerIdentity, remove_descriptor, write_descriptor,
};
use crate::ecs::{Effect, Inbound, ManagerOutcome};
use crate::ids::{PaneId, ViewerId};
use crate::os::lock;
use crate::os::pty::PaneProcess;
use crate::proto::attach::ServerMessage;
use crate::proto::control::{Event, EventCursor, EventKind, Reply, SequencedEvent};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, mpsc, oneshot};

/// Frames queued for one viewer connection. A state update with no reply behind it absorbs the
/// next one (later rows replace earlier rows, so applying the merged update equals applying
/// both); replies keep their order after the frame they promise.
#[derive(Default)]
pub struct Outbox {
    queue: VecDeque<ServerMessage>,
    closed: bool,
}

pub const OUTBOX_DEPTH: usize = 64;

#[derive(Clone, Default)]
pub struct ViewerOutbox {
    inner: Arc<Mutex<Outbox>>,
    notify: Arc<Notify>,
}

impl ViewerOutbox {
    /// Returns false when the viewer is too slow and must be disconnected.
    pub fn push(&self, message: ServerMessage) -> bool {
        let mut outbox = lock(&self.inner);
        if outbox.closed {
            return false;
        }
        let message = match message {
            ServerMessage::State { state: newer } => {
                if let Some(ServerMessage::State { state: queued }) = outbox.queue.back_mut() {
                    queued.merge(*newer);
                    self.notify.notify_one();
                    return true;
                }
                ServerMessage::State { state: newer }
            }
            other => other,
        };
        if outbox.queue.len() >= OUTBOX_DEPTH {
            outbox.closed = true;
            outbox.queue.clear();
            self.notify.notify_one();
            return false;
        }
        outbox.queue.push_back(message);
        self.notify.notify_one();
        true
    }

    pub fn close(&self) {
        lock(&self.inner).closed = true;
        self.notify.notify_one();
    }

    /// Next message to write, or `None` once closed and drained.
    pub async fn next(&self) -> Option<ServerMessage> {
        loop {
            {
                let mut outbox = lock(&self.inner);
                if let Some(message) = outbox.queue.pop_front() {
                    return Some(message);
                }
                if outbox.closed {
                    return None;
                }
            }
            self.notify.notified().await;
        }
    }

    #[cfg(test)]
    pub fn queued(&self) -> usize {
        lock(&self.inner).queue.len()
    }
}

/// A control-event subscriber: bounded queue, disconnects on any overflow.
pub struct Subscriber {
    pub filters: Vec<EventKind>,
    pub sender: mpsc::Sender<QueuedEvent>,
    pub bytes: Arc<AtomicUsize>,
}

pub struct QueuedEvent {
    pub entry: Arc<SequencedEvent>,
    size: usize,
    bytes: Arc<AtomicUsize>,
}

impl Drop for QueuedEvent {
    fn drop(&mut self) {
        self.bytes.fetch_sub(self.size, Ordering::AcqRel);
    }
}

/// Sockets and descriptor for one open workspace; dropping it closes the listeners.
pub struct OpenWorkspace {
    pub name: String,
    pub descriptor: Descriptor,
    pub stop: Arc<Notify>,
    pub subscribers: Arc<Mutex<Vec<Subscriber>>>,
}

pub struct Adapter {
    pub paths: DaemonPaths,
    pub identity: ManagerIdentity,
    pub events: mpsc::Sender<Inbound>,
    panes: HashMap<PaneId, PaneProcess>,
    pub viewers: HashMap<ViewerId, ViewerOutbox>,
    pub control_replies: HashMap<u64, oneshot::Sender<Reply>>,
    pub manager_replies: HashMap<u64, oneshot::Sender<ManagerOutcome>>,
    pub workspaces: BTreeMap<String, OpenWorkspace>,
    pub spawns: tokio::task::JoinSet<(PaneId, Result<PaneProcess, String>)>,
    pub blocking: tokio::task::JoinSet<()>,
    pub idle: bool,
    pub opened: Vec<(String, u64)>,
    pub closed: Vec<String>,
}

impl Adapter {
    pub fn new(
        paths: DaemonPaths,
        identity: ManagerIdentity,
        events: mpsc::Sender<Inbound>,
    ) -> Self {
        Self {
            paths,
            identity,
            events,
            panes: HashMap::new(),
            viewers: HashMap::new(),
            control_replies: HashMap::new(),
            manager_replies: HashMap::new(),
            workspaces: BTreeMap::new(),
            spawns: tokio::task::JoinSet::new(),
            blocking: tokio::task::JoinSet::new(),
            idle: false,
            opened: Vec::new(),
            closed: Vec::new(),
        }
    }

    /// Applies one step's effects in order. Socket binding is left to the caller through
    /// `opened`/`closed`, which need the async runtime.
    pub fn apply(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::SpawnPane {
                    pane,
                    argv,
                    cwd,
                    env,
                    rows,
                    cols,
                } => {
                    let events = self.events.clone();
                    self.spawns.spawn_blocking(move || {
                        let result = PaneProcess::spawn(
                            pane,
                            &argv,
                            cwd.as_deref(),
                            &env,
                            rows,
                            cols,
                            events,
                        )
                        .map_err(|error| error.to_string());
                        (pane, result)
                    });
                }
                Effect::WriteTrackedInput {
                    pane,
                    operation,
                    bytes,
                } => {
                    let result = self
                        .panes
                        .get(&pane)
                        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::BrokenPipe))
                        .and_then(|process| process.write_tracked_input(operation, &bytes));
                    if let Err(error) = result {
                        let events = self.events.clone();
                        let error = error.to_string().chars().take(256).collect();
                        // Bounded by the receipt budget. Never drop an immediate queue rejection
                        // just because the owner channel is momentarily full.
                        self.blocking.spawn(async move {
                            let _ = events
                                .send(Inbound::InputCompleted {
                                    pane,
                                    operation,
                                    bytes_written: 0,
                                    error: Some(error),
                                })
                                .await;
                        });
                    }
                }
                Effect::WriteInput { pane, bytes } => {
                    if let Some(process) = self.panes.get(&pane)
                        && let Err(error) = process.write_input(&bytes)
                    {
                        tracing::debug!(pane = pane.0, %error, "pane input dropped");
                    }
                }
                Effect::ResizePty { pane, rows, cols } => {
                    if let Some(process) = self.panes.get(&pane) {
                        let _ = process.resize(rows, cols);
                    }
                }
                Effect::Terminate { pane, grace_ms } => {
                    if let Some(process) = self.panes.get(&pane) {
                        let mut group = process.group();
                        self.blocking.spawn_blocking(move || {
                            group.terminate(Duration::from_millis(grace_ms));
                        });
                    }
                }
                Effect::ReleasePane { pane } => {
                    if let Some(process) = self.panes.remove(&pane) {
                        self.blocking.spawn_blocking(move || process.join());
                    }
                }
                Effect::ToViewer { viewer, message } => {
                    if let Some(outbox) = self.viewers.get(&viewer)
                        && !outbox.push(message)
                    {
                        tracing::debug!(viewer = viewer.0, "slow viewer disconnected");
                        outbox.close();
                    }
                }
                Effect::CloseViewer { viewer } => {
                    if let Some(outbox) = self.viewers.remove(&viewer) {
                        outbox.close();
                    }
                }
                Effect::ControlReply { token, reply } => {
                    if let Some(sender) = self.control_replies.remove(&token) {
                        let _ = sender.send(reply);
                    }
                }
                Effect::Manager { token, outcome } => {
                    if let Some(sender) = self.manager_replies.remove(&token) {
                        let _ = sender.send(outcome);
                    }
                }
                Effect::Event {
                    workspace,
                    event,
                    cursor,
                    size,
                } => {
                    if let Some(open) = self.workspaces.get(&workspace) {
                        publish(&open.subscribers, &event, cursor, size);
                    }
                }
                Effect::WorkspaceOpened { name, stream } => self.opened.push((name, stream)),
                Effect::WorkspaceClosed { name } => self.closed.push(name),
                Effect::Idle => self.idle = true,
            }
        }
    }

    /// Records a finished spawn and reports it. The reader thread starts with the process, so
    /// output, EOF and even the exit may already be queued; the ECS keeps an exit that arrives
    /// before (or with) the completion.
    pub fn record_spawn(&mut self, pane: PaneId, result: Result<PaneProcess, String>) -> Inbound {
        match result {
            Ok(process) => {
                let pid = process.pid();
                self.panes.insert(pane, process);
                Inbound::SpawnCompleted {
                    pane,
                    result: Ok(pid),
                }
            }
            Err(error) => Inbound::SpawnCompleted {
                pane,
                result: Err(error),
            },
        }
    }

    pub fn descriptor_for(
        &self,
        name: &str,
        stream: u64,
    ) -> Result<Descriptor, crate::daemon::PathError> {
        descriptor(&self.paths, &self.identity, name, stream)
    }

    pub fn register_workspace(&mut self, open: OpenWorkspace) -> anyhow::Result<()> {
        write_descriptor(&self.paths, &open.descriptor)?;
        self.workspaces.insert(open.name.clone(), open);
        Ok(())
    }

    pub fn unregister_workspace(&mut self, name: &str) {
        if let Some(open) = self.workspaces.remove(name) {
            open.stop.notify_waiters();
            open.stop.notify_one();
            lock(&open.subscribers).clear();
        }
        let _ = remove_descriptor(&self.paths, name);
    }

    /// Kills and reaps everything still owned; used on server exit.
    pub async fn shutdown(mut self) {
        for (_, outbox) in self.viewers.drain() {
            outbox.close();
        }
        let names: Vec<String> = self.workspaces.keys().cloned().collect();
        for name in names {
            self.unregister_workspace(&name);
        }
        let reap = |process: PaneProcess| {
            let mut group = process.group();
            group.terminate(Duration::from_millis(200));
            process.join();
        };
        for (_, process) in self.panes.drain() {
            self.blocking.spawn_blocking(move || reap(process));
        }
        while let Some(result) = self.spawns.join_next().await {
            if let Ok((_, Ok(process))) = result {
                self.blocking.spawn_blocking(move || reap(process));
            }
        }
        while self.blocking.join_next().await.is_some() {}
    }
}

/// The descriptor viewers use to reach workspace `name` on this server.
pub fn descriptor(
    paths: &DaemonPaths,
    identity: &ManagerIdentity,
    name: &str,
    stream: u64,
) -> Result<Descriptor, crate::daemon::PathError> {
    Ok(Descriptor {
        stream,
        name: name.to_owned(),
        pid: identity.pid,
        instance_nonce: identity.instance_nonce.clone(),
        socket_path: paths.attach_socket(name)?,
    })
}

/// `encoded` is the entry's exact JSON length as computed by the event log; each queued copy is
/// charged that length plus a fixed 32-byte envelope allowance against the subscriber budget.
pub(super) fn publish(
    subscribers: &Mutex<Vec<Subscriber>>,
    event: &Event,
    cursor: EventCursor,
    encoded: usize,
) {
    let mut subscribers = lock(subscribers);
    if subscribers.is_empty() {
        return;
    }
    let kind = event.kind();
    let entry = Arc::new(SequencedEvent {
        cursor,
        event: event.clone(),
    });
    let size = encoded.saturating_add(32);
    subscribers.retain(|subscriber| {
        if !subscriber.filters.is_empty() && !subscriber.filters.contains(&kind) {
            return true;
        }
        if subscriber
            .bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(size)
                    .filter(|next| *next <= crate::ecs::events::MAX_EVENT_BYTES)
            })
            .is_err()
        {
            return false;
        }
        let queued = QueuedEvent {
            entry: Arc::clone(&entry),
            size,
            bytes: Arc::clone(&subscriber.bytes),
        };
        match subscriber.sender.try_send(queued) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => false,
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    });
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    use crate::view::FrameUpdate;

    fn sized(event: &Event, cursor: EventCursor) -> usize {
        crate::ecs::events::encoded_len(&SequencedEvent {
            cursor,
            event: event.clone(),
        })
    }

    #[test]
    fn subscriber_overflow_disconnects_and_releases_queued_bytes() {
        for byte_limit in [false, true] {
            let (sender, mut receiver) = mpsc::channel(if byte_limit { 1024 } else { 1 });
            let bytes = Arc::new(AtomicUsize::new(0));
            let subscribers = Mutex::new(vec![Subscriber {
                filters: Vec::new(),
                sender,
                bytes: Arc::clone(&bytes),
            }]);
            let event = if byte_limit {
                Event::PaneTitle {
                    id: 0,
                    pane: PaneId(1),
                    title: "x".repeat(64 * 1024),
                }
            } else {
                Event::WorkspaceChanged { id: 0 }
            };
            for sequence in 1..=10 {
                let cursor = EventCursor {
                    stream: 1,
                    sequence,
                };
                publish(&subscribers, &event, cursor, sized(&event, cursor));
            }
            assert!(lock(&subscribers).is_empty());
            assert!(bytes.load(Ordering::Acquire) > 0);
            assert!(bytes.load(Ordering::Acquire) <= crate::ecs::events::MAX_EVENT_BYTES);
            while let Ok(event) = receiver.try_recv() {
                drop(event);
            }
            assert_eq!(bytes.load(Ordering::Acquire), 0);
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::error::TryRecvError::Disconnected)
            ));
        }
    }

    #[test]
    fn subscriber_filters_do_not_consume_queue_budget() {
        let (sender, mut receiver) = mpsc::channel(1);
        let bytes = Arc::new(AtomicUsize::new(0));
        let subscribers = Mutex::new(vec![Subscriber {
            filters: vec![EventKind::PaneTitle],
            sender,
            bytes: Arc::clone(&bytes),
        }]);
        let cursor = EventCursor {
            stream: 1,
            sequence: 1,
        };
        let event = Event::WorkspaceChanged { id: 0 };
        publish(&subscribers, &event, cursor, sized(&event, cursor));
        assert_eq!(bytes.load(Ordering::Acquire), 0);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        drop(receiver);
        let cursor = EventCursor {
            stream: 1,
            sequence: 2,
        };
        let event = Event::PaneTitle {
            id: 0,
            pane: PaneId(1),
            title: "t".into(),
        };
        publish(&subscribers, &event, cursor, sized(&event, cursor));
        assert!(lock(&subscribers).is_empty());
        assert_eq!(bytes.load(Ordering::Acquire), 0);
    }

    fn frame(generation: u64) -> ServerMessage {
        ServerMessage::State {
            state: Box::new(FrameUpdate {
                generation,
                ..FrameUpdate::default()
            }),
        }
    }

    #[tokio::test]
    async fn queued_updates_merge_so_no_row_is_lost() {
        use crate::view::{FrameUpdate, Line, PaneUpdate, WireCell};
        let pane = |row: u16, text: &str| PaneUpdate {
            rows: 2,
            columns: 1,
            lines: vec![Line {
                row,
                wrapped: false,
                len: 1,
            }],
            cells: vec![WireCell {
                text: Some(text.into()),
                ..WireCell::default()
            }],
            ..PaneUpdate::default()
        };
        let update = |generation: u64, row: u16, text: &str| {
            let mut frame = FrameUpdate {
                generation,
                layout: vec![crate::view::PaneRect {
                    pane: PaneId(1),
                    rect: crate::layout::Rect::default(),
                }],
                ..FrameUpdate::default()
            };
            frame.panes.insert(PaneId(1), pane(row, text));
            ServerMessage::State {
                state: Box::new(frame),
            }
        };
        let outbox = ViewerOutbox::default();
        assert!(outbox.push(update(1, 0, "a")));
        assert!(outbox.push(update(2, 1, "b")));
        assert!(outbox.push(update(3, 0, "c")));
        assert_eq!(outbox.queued(), 1);
        let Some(ServerMessage::State { state }) = outbox.next().await else {
            panic!("a merged update");
        };
        assert_eq!(state.generation, 3);
        let Some(merged) = state.panes.get(&PaneId(1)) else {
            panic!("the merged pane");
        };
        let rows: Vec<(u16, Option<String>)> = merged
            .lines
            .iter()
            .zip(&merged.cells)
            .map(|(line, cell)| (line.row, cell.text.clone()))
            .collect();
        assert_eq!(rows, vec![(0, Some("c".into())), (1, Some("b".into()))]);
    }

    #[tokio::test]
    async fn outbox_coalesces_frames_but_keeps_replies_ordered_after_them() {
        let outbox = ViewerOutbox::default();
        assert!(outbox.push(frame(1)));
        assert!(outbox.push(frame(2)));
        assert_eq!(outbox.queued(), 1);
        assert!(outbox.push(ServerMessage::Reply {
            reply: Reply::Accepted { id: 1 }
        }));
        assert!(outbox.push(frame(3)));
        assert!(outbox.push(frame(4)));
        let mut generations = Vec::new();
        for _ in 0..3 {
            match outbox.next().await {
                Some(ServerMessage::State { state }) => generations.push(state.generation),
                Some(ServerMessage::Reply { .. }) => generations.push(0),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(generations, vec![2, 0, 4]);
        for index in 0..OUTBOX_DEPTH {
            let accepted = outbox.push(ServerMessage::Reply {
                reply: Reply::Accepted { id: index as u64 },
            });
            if !accepted {
                break;
            }
        }
        assert!(!outbox.push(frame(9)), "a slow viewer is disconnected");
        assert!(outbox.next().await.is_none());
    }
}
