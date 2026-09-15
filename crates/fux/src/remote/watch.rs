//! Streaming methods (prompt 3.9): fux's own watch loop under `RemoteHttpPlugin`'s SSE delivery.
//!
//! `RemoteHttpPlugin` selects streaming by the `+watch` substring: it hands the dispatcher a
//! `BrpMessage` whose `sender` has room for eight results and serves every result as one
//! `text/event-stream` item until the sender is closed; inside a batch it answers `-32600`
//! itself and drops the receiver, so such a message arrives here already closed. The built-in
//! loop (`bevy_remote::process_ongoing_watching_requests`) re-runs one system with the request's
//! params every tick and keeps no per-stream state; fux needs a cursor per events stream, an
//! observer per `world.observe+watch` stream and a fresh `Local` per built-in watch, so each
//! open stream is a [`Watch`] here, polled in `Last` (after the lifecycle systems of the same
//! update, so an event reaches its stream in the tick that produced it) and torn down when its
//! sender closes or its token is revoked.

use bevy_ecs::prelude::*;
use bevy_ecs::reflect::{AppTypeRegistry, ReflectEvent};
use bevy_ecs::world::DeferredWorld;
use bevy_platform::collections::HashMap;
use bevy_reflect::Reflect;
use bevy_reflect::serde::ReflectSerializer;
use bevy_remote::builtin_methods::{
    self, BRP_GET_COMPONENTS_AND_WATCH_METHOD, BRP_LIST_COMPONENTS_AND_WATCH_METHOD,
    BRP_OBSERVE_METHOD, BrpGetComponentsParams, BrpListComponentsParams, BrpObserveParams,
};
use bevy_remote::{BrpError, BrpMessage, BrpResult, RemoteWatchingMethodSystemId, error_codes};
use serde_json::{Value, json};
use std::collections::VecDeque;

use super::methods::{Request, check_paths, invalid, to_value};
use super::projection::is_allowed_type_path;
use super::schema::described;
use crate::events::{EVENT_TYPE_PATHS, Entry, EventLog, Gap};
use crate::model::ServerInstance;

pub const EVENTS_WATCH_METHOD: &str = "fux/events+watch";

/// Bound on open streams server-wide; each holds a channel, and an observer entity or a
/// registered system.
pub const MAX_WATCHES: usize = 256;
/// Event bodies an `observe+watch` stream may hold between two deliveries; beyond it the
/// oldest is dropped and the next item reports how many.
pub const OBSERVE_BUFFER: usize = 256;

/// Opens one stream: validates the (already authorised) request and returns its state.
pub type Open = fn(&mut Request, &mut World) -> Result<State, BrpError>;

/// Streams under Bevy's standard names, wrapped over the projection vocabulary.
pub static WATCHED: &[(&str, Open)] = &[
    (BRP_OBSERVE_METHOD, open_observe),
    (BRP_GET_COMPONENTS_AND_WATCH_METHOD, open_get_components),
    (BRP_LIST_COMPONENTS_AND_WATCH_METHOD, open_list_components),
];

described!(
    pub struct EventsWatchParams {
        /// Defaults to the token's workspace; an unscoped token without it streams every
        /// workspace, ordered by cursor.
        pub workspace: Option<String>,
        /// The last cursor seen; absent starts at the present. Needs `instance`.
        pub cursor: Option<u64>,
    }
);
described!(
    pub struct GapNotice {
        pub since: u64,
        pub resume: u64,
    }
);
described!(
    pub struct EventRecord {
        pub cursor: u64,
        pub workspace: String,
        pub ms: u64,
        pub name: String,
        pub event: Value,
    }
);
described!(
    /// One stream item: a `gap` (with no events) when retention outran the cursor, otherwise
    /// the events since the last item.
    pub struct EventsWatchItem {
        pub gap: Option<GapNotice>,
        pub events: Vec<EventRecord>,
    }
);
described!(
    pub struct ObserveItem {
        pub events: Vec<Value>,
        pub dropped: u64,
    }
);

/// Per-stream state, owned by the loop rather than the handler.
pub enum State {
    Events {
        /// `None`: every workspace the token covers (an unscoped grant).
        workspace: Option<String>,
        /// Last delivered cursor.
        cursor: u64,
        /// Reported before the next events.
        gap: Option<GapNotice>,
    },
    Observe {
        /// The observer entity, carrying its [`ObserveBuffer`]; despawned with the stream.
        observer: Entity,
    },
    Builtin {
        /// Registered per stream so the built-in's `Local` removal cursors are its own.
        system: RemoteWatchingMethodSystemId,
        params: Value,
        /// Cuts the built-in's result down to the projection vocabulary.
        filter: fn(Value) -> Option<Value>,
    },
}

struct Watch {
    sender: async_channel::Sender<BrpResult>,
    token: String,
    state: State,
}

/// Open streams plus the opener per streaming method name.
#[derive(Resource, Default)]
pub struct Watches {
    openers: HashMap<&'static str, Open>,
    open: Vec<Watch>,
}

impl Watches {
    pub fn register(&mut self, name: &'static str, open: Open) {
        self.openers.insert(name, open);
    }

    pub fn open_count(&self) -> usize {
        self.open.len()
    }
}

/// Bounded queue of reflected event bodies on an `observe+watch` observer entity.
#[derive(Component, Default)]
pub struct ObserveBuffer {
    items: VecDeque<Value>,
    dropped: u64,
}

impl ObserveBuffer {
    fn push(&mut self, value: Value) {
        if self.items.len() == OBSERVE_BUFFER {
            self.items.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.items.push_back(value);
    }
}

// ---------------------------------------------------------------------------------------------
// Opening
// ---------------------------------------------------------------------------------------------

/// The `RemoteMethods` entry of every stream. `bevy_remote`'s loop would re-run it each tick;
/// fux's dispatcher never does, since streams are opened by name through [`Watches`].
pub fn placeholder(In(_): In<Option<Value>>) -> BrpResult<Option<Value>> {
    Err(BrpError {
        code: error_codes::INTERNAL_ERROR,
        message: "streams are served by fux's watch loop".into(),
        data: None,
    })
}

/// Opens the stream a `+watch` message asks for, or answers it with the error.
pub fn open(world: &mut World, message: BrpMessage) {
    let BrpMessage {
        method,
        params,
        sender,
    } = message;
    // A batch member: `RemoteHttpPlugin` answered `-32600` and dropped the receiver already.
    if sender.is_closed() {
        return;
    }
    let result = open_state(world, &method, params);
    match result {
        Ok((token, state)) => world.resource_mut::<Watches>().open.push(Watch {
            sender,
            token,
            state,
        }),
        Err(error) => {
            let _ = sender.force_send(Err(error));
        }
    }
}

fn open_state(
    world: &mut World,
    method: &str,
    params: Option<Value>,
) -> Result<(String, State), BrpError> {
    let opener = world
        .resource::<Watches>()
        .openers
        .get(method)
        .copied()
        .ok_or_else(|| BrpError {
            code: error_codes::METHOD_NOT_FOUND,
            message: format!("Method `{method}` not found"),
            data: None,
        })?;
    if world.resource::<Watches>().open.len() >= MAX_WATCHES {
        return Err(invalid("watch limit reached"));
    }
    let mut req = Request::open(params, world)?;
    let state = opener(&mut req, world)?;
    Ok((req.token().to_owned(), state))
}

pub(super) fn open_events(req: &mut Request, world: &mut World) -> Result<State, BrpError> {
    let params: EventsWatchParams = req.parse()?;
    let workspace = match (params.workspace, req.workspace_scope()) {
        (Some(named), _) => {
            req.cover_name(&named)?;
            Some(named)
        }
        (None, Some(scope)) => Some(scope.to_owned()),
        (None, None) => None,
    };
    let log = world.resource::<EventLog>();
    let (cursor, gap) = match params.cursor {
        None => (
            workspace
                .as_deref()
                .map_or_else(|| log.latest(), |ws| log.cursor(ws)),
            None,
        ),
        Some(cursor) => {
            let nonce = &world.resource::<ServerInstance>().nonce;
            match req.instance() {
                Some(instance) if instance == nonce => {
                    resolve_cursor(log, workspace.as_deref(), cursor)
                }
                // Another incarnation's cursor: everything retained is news.
                Some(_) => foreign_cursor(log, workspace.as_deref(), cursor),
                None => return Err(invalid("a cursor needs `instance`")),
            }
        }
    };
    Ok(State::Events {
        workspace,
        cursor,
        gap,
    })
}

/// Where a resumed stream starts: at `cursor` when it is retained, else at the log's resume
/// point with the gap to report first.
fn resolve_cursor(
    log: &EventLog,
    workspace: Option<&str>,
    cursor: u64,
) -> (u64, Option<GapNotice>) {
    let probe = match workspace {
        Some(ws) => log.read_after(ws, cursor).map(|_| ()),
        None => log.read_any_after(cursor, &mut Vec::new()),
    };
    match probe {
        Ok(()) => (cursor, None),
        Err(gap) => (gap.resume, Some(notice(gap))),
    }
}

fn foreign_cursor(log: &EventLog, workspace: Option<&str>, since: u64) -> (u64, Option<GapNotice>) {
    let (resume, _) = resolve_cursor(log, workspace, 0);
    (resume, Some(GapNotice { since, resume }))
}

fn notice(gap: Gap) -> GapNotice {
    GapNotice {
        since: gap.since,
        resume: gap.resume,
    }
}

fn open_observe(req: &mut Request, world: &mut World) -> Result<State, BrpError> {
    let BrpObserveParams { event, entity } = req.parse()?;
    if entity.is_some() {
        return Err(invalid(
            "entity scoping is not available; event bodies carry public ids",
        ));
    }
    if !EVENT_TYPE_PATHS.contains(&event.as_str()) {
        return Err(super::methods::unauthorized(format!(
            "`{event}` is not a public lifecycle event; see fux::events"
        )));
    }
    let registry = world.resource::<AppTypeRegistry>().clone();
    let reflect_event = {
        let types = registry.read();
        types
            .get_with_type_path(&event)
            .and_then(|registration| registration.data::<ReflectEvent>())
            .cloned()
            .ok_or_else(|| BrpError::internal(format!("`{event}` is not a reflected event")))?
    };
    let observer = world.spawn(ObserveBuffer::default()).id();
    let callback = Box::new(move |body: &dyn Reflect, mut world: DeferredWorld| {
        let types = registry.read();
        let serialized = serde_json::to_value(ReflectSerializer::new(body, &types));
        // `ReflectSerializer` wraps the body in `{ "<type path>": body }`.
        let body = match serialized {
            Ok(Value::Object(mut wrapped)) => wrapped
                .remove(&event)
                .unwrap_or_else(|| json!({ "event": event })),
            Ok(other) => other,
            Err(_) => json!({ "event": event }),
        };
        if let Some(mut buffer) = world.get_mut::<ObserveBuffer>(observer) {
            buffer.push(body);
        }
    });
    world
        .entity_mut(observer)
        .insert(reflect_event.create_observer(callback));
    Ok(State::Observe { observer })
}

fn open_get_components(req: &mut Request, world: &mut World) -> Result<State, BrpError> {
    let params: BrpGetComponentsParams = req.parse()?;
    check_paths(&params.components)?;
    Ok(State::Builtin {
        system: world
            .register_system(builtin_methods::process_remote_get_components_watching_request),
        params: to_value(params)?,
        filter: Some,
    })
}

fn open_list_components(req: &mut Request, world: &mut World) -> Result<State, BrpError> {
    let params: BrpListComponentsParams = req.parse()?;
    Ok(State::Builtin {
        system: world
            .register_system(builtin_methods::process_remote_list_components_watching_request),
        params: to_value(params)?,
        filter: filter_listed,
    })
}

/// Keeps only projection paths in `added`/`removed`; nothing left means no item.
fn filter_listed(value: Value) -> Option<Value> {
    let Value::Object(mut fields) = value else {
        return Some(value);
    };
    let mut any = false;
    for key in ["added", "removed"] {
        if let Some(Value::Array(names)) = fields.get_mut(key) {
            names.retain(|n| n.as_str().is_some_and(is_allowed_type_path));
            any |= !names.is_empty();
        }
    }
    any.then_some(Value::Object(fields))
}

// ---------------------------------------------------------------------------------------------
// Polling and closing
// ---------------------------------------------------------------------------------------------

/// `Last`: delivers what every open stream has to say and tears down closed ones.
pub fn poll(world: &mut World) {
    let mut watches = core::mem::take(&mut world.resource_mut::<Watches>().open);
    watches.retain_mut(|watch| {
        if watch.sender.is_closed() {
            close(world, watch);
            return false;
        }
        let result = match &mut watch.state {
            State::Events {
                workspace,
                cursor,
                gap,
            } => {
                // Retained entries wait for the channel; nothing is lost by skipping a tick.
                if watch.sender.is_full() {
                    return true;
                }
                poll_events(world, workspace.as_deref(), cursor, gap)
            }
            State::Observe { observer } => {
                if watch.sender.is_full() {
                    return true;
                }
                poll_observe(world, *observer)
            }
            State::Builtin {
                system,
                params,
                filter,
            } => poll_builtin(world, *system, params, *filter),
        };
        let Some(result) = result else {
            return true;
        };
        if watch.sender.try_send(result).is_err() {
            // Gone or (built-in only) too slow: the stream ends.
            close(world, watch);
            return false;
        }
        true
    });
    world.resource_mut::<Watches>().open = watches;
}

fn poll_events(
    world: &World,
    workspace: Option<&str>,
    cursor: &mut u64,
    gap: &mut Option<GapNotice>,
) -> Option<BrpResult> {
    if let Some(gap) = gap.take() {
        return Some(to_value(EventsWatchItem {
            gap: Some(gap),
            events: Vec::new(),
        }));
    }
    let log = world.resource::<EventLog>();
    let (events, missed) = match workspace {
        Some(ws) => match log.read_after(ws, *cursor) {
            Ok(entries) => (entries.iter().map(record).collect::<Vec<_>>(), None),
            Err(gap) => (Vec::new(), Some(gap)),
        },
        None => {
            let mut merged = Vec::new();
            match log.read_any_after(*cursor, &mut merged) {
                Ok(()) => (merged.into_iter().map(record).collect(), None),
                Err(gap) => (Vec::new(), Some(gap)),
            }
        }
    };
    if let Some(missed) = missed {
        *cursor = missed.resume;
        return Some(to_value(EventsWatchItem {
            gap: Some(notice(missed)),
            events: Vec::new(),
        }));
    }
    let last = events.last()?.cursor;
    *cursor = last;
    Some(to_value(EventsWatchItem { gap: None, events }))
}

fn record(entry: &Entry) -> EventRecord {
    EventRecord {
        cursor: entry.cursor,
        workspace: entry.workspace.clone(),
        ms: entry.ms,
        name: entry.name.to_owned(),
        event: entry.event.clone(),
    }
}

fn poll_observe(world: &mut World, observer: Entity) -> Option<BrpResult> {
    let mut buffer = world.get_mut::<ObserveBuffer>(observer)?;
    if buffer.items.is_empty() {
        return None;
    }
    let events: Vec<Value> = buffer.items.drain(..).collect();
    let dropped = core::mem::take(&mut buffer.dropped);
    Some(to_value(ObserveItem { events, dropped }))
}

fn poll_builtin(
    world: &mut World,
    system: RemoteWatchingMethodSystemId,
    params: &Value,
    filter: fn(Value) -> Option<Value>,
) -> Option<BrpResult> {
    match world.run_system_with(system, Some(params.clone())) {
        Ok(Ok(Some(value))) => filter(value).map(Ok),
        Ok(Ok(None)) => None,
        Ok(Err(error)) => Some(Err(error)),
        Err(error) => Some(Err(BrpError {
            code: error_codes::INTERNAL_ERROR,
            message: format!("failed to run method handler: {error}"),
            data: None,
        })),
    }
}

fn close(world: &mut World, watch: &Watch) {
    match &watch.state {
        State::Events { .. } => {}
        State::Observe { observer } => {
            world.despawn(*observer);
        }
        State::Builtin { system, .. } => {
            let _ = world.unregister_system(*system);
        }
    }
    watch.sender.close();
}

/// Ends every stream the token opened; called by `fux/token.revoke` after the table forgot it.
pub fn revoke(world: &mut World, token: &str) {
    let mut watches = core::mem::take(&mut world.resource_mut::<Watches>().open);
    watches.retain(|watch| {
        if super::token::constant_time_eq(watch.token.as_bytes(), token.as_bytes()) {
            close(world, watch);
            false
        } else {
            true
        }
    });
    world.resource_mut::<Watches>().open = watches;
}
