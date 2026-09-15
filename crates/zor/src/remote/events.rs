//! Public lifecycle events and the retained event log behind `zor/events+watch`.
//!
//! Every lifecycle fact is an `EntityEvent` targeting the entity it is about, reflected with
//! `#[reflect(Event)]`; bodies carry public ids only. The lifecycle triggers them after a
//! transition commits; the one consumer here is [`EventLog`]: one bounded server-wide stream
//! with an explicit [`Gap`] once eviction outran a reader's cursor. fux's `EventLog` is
//! per-workspace with retirement semantics zor has no use for, hence this smaller copy.

use std::collections::VecDeque;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_reflect::prelude::*;
use serde::Serialize;
use serde_json::Value;

use crate::model::{
    AttemptId, AttemptState, CheckId, CheckState, Clock, Delivery, Limits, PromptId, TaskId,
    TaskOutcome,
};

/// Bytes of event bodies retained (the old protocol's 512 KiB).
pub const MAX_LOG_BYTES: usize = 512 * 1024;

/// What the log needs from an event beyond its body.
pub trait Logged: EntityEvent + Serialize {
    /// The `name` of the log entry: the bare type name.
    const NAME: &'static str;
}

macro_rules! lifecycle_event {
    ($(#[$m:meta])* $name:ident { $($(#[$fm:meta])* $field:ident : $ty:ty),* $(,)? }) => {
        $(#[$m])*
        #[derive(EntityEvent, Reflect, Clone, Debug, PartialEq, Serialize)]
        #[reflect(Event, Clone, from_reflect = false)]
        pub struct $name {
            /// The entity the fact is about; not part of the body.
            #[reflect(ignore)]
            #[serde(skip)]
            pub entity: Entity,
            $($(#[$fm])* pub $field: $ty,)*
        }

        impl Logged for $name {
            const NAME: &'static str = stringify!($name);
        }
    };
}

lifecycle_event!(TaskOpened { task: TaskId });
lifecycle_event!(TaskClosed {
    task: TaskId,
    outcome: TaskOutcome
});
lifecycle_event!(AttemptChanged {
    attempt: AttemptId,
    state: AttemptState
});
lifecycle_event!(PromptChanged {
    prompt: PromptId,
    delivery: Delivery
});
lifecycle_event!(CheckChanged {
    check: CheckId,
    state: CheckState
});

/// Reflected type paths of the public events.
pub const EVENT_TYPE_PATHS: [&str; 5] = [
    "zor::remote::events::TaskOpened",
    "zor::remote::events::TaskClosed",
    "zor::remote::events::AttemptChanged",
    "zor::remote::events::PromptChanged",
    "zor::remote::events::CheckChanged",
];

/// One retained event.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Entry {
    /// Server-wide monotonic cursor, starting at 1.
    pub cursor: u64,
    /// `Clock` milliseconds when the event was logged.
    pub ms: u64,
    pub name: &'static str,
    pub event: Value,
}

/// A cursor the log cannot serve losslessly; `resume` is the smallest cursor now accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Gap {
    pub since: u64,
    pub resume: u64,
}

/// The retained log: bounded by entries and bytes, explicit gaps once eviction happened.
#[derive(Resource, Debug)]
pub struct EventLog {
    entries: VecDeque<Entry>,
    /// Next cursor to issue.
    next: u64,
    /// Highest cursor ever evicted: the floor of lossless reads.
    floor: u64,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl EventLog {
    pub fn new(limits: &Limits) -> Self {
        Self {
            entries: VecDeque::new(),
            next: 1,
            floor: 0,
            bytes: 0,
            max_entries: limits.event_log_entries.max(1),
            max_bytes: MAX_LOG_BYTES,
        }
    }

    /// Every retained entry after `cursor` (the last one the reader saw; `latest()` for "from
    /// now").
    pub fn read_after(&self, cursor: u64) -> Result<impl Iterator<Item = &Entry>, Gap> {
        if cursor >= self.next {
            return Err(Gap {
                since: cursor,
                resume: self.latest(),
            });
        }
        if cursor < self.floor {
            return Err(Gap {
                since: cursor,
                resume: self.floor,
            });
        }
        let from = self.entries.partition_point(|e| e.cursor <= cursor);
        Ok(self.entries.range(from..))
    }

    /// The latest cursor issued.
    pub fn latest(&self) -> u64 {
        self.next - 1
    }

    pub fn retained(&self) -> usize {
        self.entries.len()
    }

    pub fn append<E: Logged>(&mut self, event: &E, ms: u64) {
        match serde_json::to_value(event) {
            Ok(body) => self.push(E::NAME, body, ms),
            Err(error) => bevy_log::warn!("serialize {}: {error}", E::NAME),
        }
    }

    pub fn push(&mut self, name: &'static str, event: Value, ms: u64) {
        let cursor = self.next;
        self.next += 1;
        let entry = Entry {
            cursor,
            ms,
            name,
            event,
        };
        self.bytes += weight(&entry);
        self.entries.push_back(entry);
        while self.entries.len() > self.max_entries
            || (self.bytes > self.max_bytes && self.entries.len() > 1)
        {
            let Some(evicted) = self.entries.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(weight(&evicted));
            self.floor = evicted.cursor;
        }
    }
}

/// Approximate JSON size of an entry, for the byte bound.
fn weight(entry: &Entry) -> usize {
    fn json(value: &Value) -> usize {
        match value {
            Value::Null => 4,
            Value::Bool(_) => 5,
            Value::Number(_) => 20,
            Value::String(s) => s.len() + 2,
            Value::Array(items) => 2 + items.iter().map(|v| json(v) + 1).sum::<usize>(),
            Value::Object(map) => {
                2 + map
                    .iter()
                    .map(|(k, v)| k.len() + 4 + json(v))
                    .sum::<usize>()
            }
        }
    }
    40 + entry.name.len() + json(&entry.event)
}

/// Registers the event types and the global observers that append them to the log.
pub fn register(app: &mut App) {
    app.register_type::<TaskOpened>()
        .register_type::<TaskClosed>()
        .register_type::<AttemptChanged>()
        .register_type::<PromptChanged>()
        .register_type::<CheckChanged>()
        .add_observer(log_event::<TaskOpened>)
        .add_observer(log_event::<TaskClosed>)
        .add_observer(log_event::<AttemptChanged>)
        .add_observer(log_event::<PromptChanged>)
        .add_observer(log_event::<CheckChanged>);
}

fn log_event<E: Logged>(event: On<E>, mut log: ResMut<EventLog>, clock: Res<Clock>) {
    log.append(event.event(), clock.now_ms);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eviction_reports_a_gap_and_resumes_at_the_floor() {
        let limits = Limits {
            event_log_entries: 2,
            ..Limits::default()
        };
        let mut log = EventLog::new(&limits);
        assert!(log.read_after(0).unwrap().next().is_none());
        for i in 0..3 {
            log.push("E", serde_json::json!({ "i": i }), i);
        }
        assert_eq!(log.latest(), 3);
        let gap = log.read_after(0).err().unwrap();
        assert_eq!(
            gap,
            Gap {
                since: 0,
                resume: 1
            }
        );
        let cursors: Vec<u64> = log.read_after(1).unwrap().map(|e| e.cursor).collect();
        assert_eq!(cursors, [2, 3]);
        assert!(log.read_after(3).unwrap().next().is_none());
        assert_eq!(log.read_after(9).err().unwrap().resume, 3);
    }
}
