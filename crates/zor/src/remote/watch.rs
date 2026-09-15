//! Streaming methods: zor's own watch loop under `RemoteHttpPlugin`'s SSE delivery (fux's
//! shape). `RemoteHttpPlugin` selects streaming by the `+watch` substring and serves every
//! result on the message's sender as one `text/event-stream` item; each open stream is a
//! [`Watch`] here, polled in `Last` after the lifecycle systems of the same update and torn
//! down when its sender closes or its token is revoked.

use bevy_ecs::prelude::*;
use bevy_platform::collections::HashMap;
use bevy_remote::builtin_methods::{
    self, BRP_GET_COMPONENTS_AND_WATCH_METHOD, BRP_LIST_COMPONENTS_AND_WATCH_METHOD,
    BrpGetComponentsParams, BrpListComponentsParams,
};
use bevy_remote::{BrpError, BrpMessage, BrpResult, RemoteWatchingMethodSystemId, error_codes};
use serde_json::Value;

use super::events::{Entry, EventLog, Gap};
use super::methods::{Request, check_paths, described, invalid, to_value};
use super::projection::is_allowed_type_path;
use crate::model::ServerInstance;

pub const EVENTS_WATCH_METHOD: &str = "zor/events+watch";

/// Bound on open streams server-wide.
pub const MAX_WATCHES: usize = 256;

/// Opens one stream: validates the (already authorised) request and returns its state.
pub type Open = fn(&mut Request, &mut World) -> Result<State, BrpError>;

/// Streams under Bevy's standard names, wrapped over the projection vocabulary.
pub static WATCHED: &[(&str, Open)] = &[
    (BRP_GET_COMPONENTS_AND_WATCH_METHOD, open_get_components),
    (BRP_LIST_COMPONENTS_AND_WATCH_METHOD, open_list_components),
];

described!(
    pub struct EventsWatchParams {
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

/// Per-stream state, owned by the loop rather than the handler.
pub enum State {
    Events {
        /// Last delivered cursor.
        cursor: u64,
        /// Reported before the next events.
        gap: Option<GapNotice>,
    },
    Builtin {
        /// Registered per stream so the built-in's `Local` removal cursors are its own.
        system: RemoteWatchingMethodSystemId,
        params: Value,
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

/// The `RemoteMethods` entry of every stream; never run, since streams are opened by name
/// through [`Watches`].
pub fn placeholder(In(_): In<Option<Value>>) -> BrpResult<Option<Value>> {
    Err(BrpError {
        code: error_codes::INTERNAL_ERROR,
        message: "streams are served by zor's watch loop".into(),
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
    match open_state(world, &method, params) {
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
    let log = world.resource::<EventLog>();
    let (cursor, gap) = match params.cursor {
        None => (log.latest(), None),
        Some(cursor) => {
            let nonce = &world.resource::<ServerInstance>().nonce;
            match req.instance() {
                Some(instance) if instance == nonce => resolve_cursor(log, cursor),
                // Another incarnation's cursor: everything retained is news.
                Some(_) => {
                    let (resume, _) = resolve_cursor(log, 0);
                    (
                        resume,
                        Some(GapNotice {
                            since: cursor,
                            resume,
                        }),
                    )
                }
                None => return Err(invalid("a cursor needs `instance`")),
            }
        }
    };
    Ok(State::Events { cursor, gap })
}

/// Where a resumed stream starts: at `cursor` when it is retained, else at the log's resume
/// point with the gap to report first.
fn resolve_cursor(log: &EventLog, cursor: u64) -> (u64, Option<GapNotice>) {
    match log.read_after(cursor) {
        Ok(_) => (cursor, None),
        Err(gap) => (gap.resume, Some(notice(gap))),
    }
}

fn notice(gap: Gap) -> GapNotice {
    GapNotice {
        since: gap.since,
        resume: gap.resume,
    }
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

/// `Last`: delivers what every open stream has to say and tears down closed ones.
pub fn poll(world: &mut World) {
    let mut watches = core::mem::take(&mut world.resource_mut::<Watches>().open);
    watches.retain_mut(|watch| {
        if watch.sender.is_closed() {
            close(world, watch);
            return false;
        }
        let result = match &mut watch.state {
            State::Events { cursor, gap } => {
                // Retained entries wait for the channel; nothing is lost by skipping a tick.
                if watch.sender.is_full() {
                    return true;
                }
                poll_events(world, cursor, gap)
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
            close(world, watch);
            return false;
        }
        true
    });
    world.resource_mut::<Watches>().open = watches;
}

fn poll_events(world: &World, cursor: &mut u64, gap: &mut Option<GapNotice>) -> Option<BrpResult> {
    if let Some(gap) = gap.take() {
        return Some(to_value(EventsWatchItem {
            gap: Some(gap),
            events: Vec::new(),
        }));
    }
    let log = world.resource::<EventLog>();
    let events: Vec<EventRecord> = match log.read_after(*cursor) {
        Ok(entries) => entries.map(record).collect(),
        Err(missed) => {
            *cursor = missed.resume;
            return Some(to_value(EventsWatchItem {
                gap: Some(notice(missed)),
                events: Vec::new(),
            }));
        }
    };
    let last = events.last()?.cursor;
    *cursor = last;
    Some(to_value(EventsWatchItem { gap: None, events }))
}

fn record(entry: &Entry) -> EventRecord {
    EventRecord {
        cursor: entry.cursor,
        ms: entry.ms,
        name: entry.name.to_owned(),
        event: entry.event.clone(),
    }
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
    if let State::Builtin { system, .. } = &watch.state {
        let _ = world.unregister_system(*system);
    }
    watch.sender.close();
}

/// Ends every stream the token opened; called by `zor/token.revoke` after the table forgot it.
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
