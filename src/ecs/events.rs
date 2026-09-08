//! Bounded, authoritative event evidence for each workspace lifetime.
use crate::proto::control::{Event, EventCursor, SequencedEvent};
use std::collections::VecDeque;

pub const MAX_EVENTS: usize = 1024;
pub const MAX_EVENT_BYTES: usize = 512 * 1024;

#[derive(Debug)]
pub struct EventLog {
    cursor: EventCursor,
    floor: u64,
    bytes: usize,
    exhausted: bool,
    records: VecDeque<(SequencedEvent, usize)>,
}

impl EventLog {
    pub fn new(stream: u64) -> Self {
        Self {
            cursor: EventCursor {
                stream,
                sequence: 0,
            },
            floor: 0,
            bytes: 0,
            exhausted: false,
            records: VecDeque::new(),
        }
    }

    pub fn cursor(&self) -> EventCursor {
        self.cursor
    }

    pub fn push(&mut self, event: Event) -> Option<SequencedEvent> {
        let Some(sequence) = self.cursor.sequence.checked_add(1) else {
            self.exhausted = true;
            return None;
        };
        self.cursor.sequence = sequence;
        let entry = SequencedEvent {
            cursor: self.cursor,
            event,
        };
        let size = serde_json::to_vec(&entry).map_or(usize::MAX, |bytes| bytes.len());
        if size > MAX_EVENT_BYTES {
            self.records.clear();
            self.bytes = 0;
            self.floor = sequence;
        } else {
            while self.records.len() >= MAX_EVENTS || self.bytes + size > MAX_EVENT_BYTES {
                let Some((old, bytes)) = self.records.pop_front() else {
                    break;
                };
                self.bytes -= bytes;
                self.floor = old.cursor.sequence;
            }
            self.bytes += size;
            self.records.push_back((entry.clone(), size));
        }
        Some(entry)
    }

    /// None means a different stream, an evicted interval, or a cursor never issued here.
    pub fn replay(&self, after: EventCursor) -> Option<Vec<SequencedEvent>> {
        if self.exhausted
            || after.stream != self.cursor.stream
            || after.sequence < self.floor
            || after.sequence > self.cursor.sequence
        {
            return None;
        }
        Some(
            self.records
                .iter()
                .filter(|(entry, _)| entry.cursor.sequence > after.sequence)
                .map(|(entry, _)| entry.clone())
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_detects_eviction_future_and_foreign_cursors() {
        let mut log = EventLog::new(7);
        let start = log.cursor();
        for _ in 0..=MAX_EVENTS {
            assert!(log.push(Event::WorkspaceChanged { id: 0 }).is_some());
        }
        assert!(log.replay(start).is_none());
        let retained = EventCursor {
            stream: 7,
            sequence: 1,
        };
        assert_eq!(
            log.replay(retained).map(|entries| entries.len()),
            Some(MAX_EVENTS)
        );
        assert_eq!(log.replay(log.cursor()), Some(Vec::new()));
        assert!(
            log.replay(EventCursor {
                stream: 8,
                sequence: 1
            })
            .is_none()
        );
        assert!(
            log.replay(EventCursor {
                stream: 7,
                sequence: u64::MAX
            })
            .is_none()
        );
    }

    #[test]
    fn byte_eviction_and_unrecordable_events_preserve_explicit_gaps() {
        let mut log = EventLog::new(1);
        let start = log.cursor();
        for _ in 0..10 {
            let _ = log.push(Event::PaneTitle {
                id: 0,
                pane: crate::ids::PaneId(1),
                title: "t".repeat(64 * 1024),
            });
        }
        assert!(log.bytes <= MAX_EVENT_BYTES);
        assert!(log.replay(start).is_none());
        let before_large = log.cursor();
        let _ = log.push(Event::PaneTitle {
            id: 0,
            pane: crate::ids::PaneId(1),
            title: "t".repeat(MAX_EVENT_BYTES + 1),
        });
        assert!(log.replay(before_large).is_none());
        assert_eq!(log.replay(log.cursor()), Some(Vec::new()));
        log.cursor.sequence = u64::MAX;
        assert!(log.push(Event::WorkspaceChanged { id: 0 }).is_none());
        assert!(log.replay(log.cursor()).is_none());
    }

    #[test]
    fn sequenced_event_wire_shape_round_trips() {
        let entry = SequencedEvent {
            cursor: EventCursor {
                stream: 1,
                sequence: 2,
            },
            event: Event::WorkspaceChanged { id: 3 },
        };
        let encoded = serde_json::to_vec(&entry).unwrap_or_default();
        assert_eq!(
            serde_json::from_slice::<SequencedEvent>(&encoded).ok(),
            Some(entry)
        );
    }
}
