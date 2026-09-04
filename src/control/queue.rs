use super::protocol::Event;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

#[derive(Clone)]
pub struct EventQueue {
    inner: Arc<Inner>,
}

pub struct EventReceiver {
    inner: Arc<Inner>,
}

struct Inner {
    state: Mutex<State>,
    changed: Condvar,
}

struct State {
    queue: VecDeque<Event>,
    capacity: usize,
    disconnected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishOutcome {
    Queued,
    DroppedOutput,
    DisconnectedSlowClient,
    Disconnected,
}

impl EventQueue {
    pub fn bounded(capacity: usize) -> Option<(Self, EventReceiver)> {
        if capacity == 0 || capacity > super::MAX_SUBSCRIBER_QUEUE {
            return None;
        }
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                queue: VecDeque::with_capacity(capacity),
                capacity,
                disconnected: false,
            }),
            changed: Condvar::new(),
        });
        Some((
            Self {
                inner: Arc::clone(&inner),
            },
            EventReceiver { inner },
        ))
    }

    pub fn publish(&self, event: Event) -> PublishOutcome {
        let mut state = recover(self.inner.state.lock());
        if state.disconnected {
            return PublishOutcome::Disconnected;
        }
        if state.queue.len() == state.capacity {
            if matches!(event, Event::PaneOutput { .. }) {
                return PublishOutcome::DroppedOutput;
            }
            if let Some(position) = state
                .queue
                .iter()
                .position(|queued| matches!(queued, Event::PaneOutput { .. }))
            {
                state.queue.remove(position);
            } else {
                state.disconnected = true;
                state.queue.clear();
                self.inner.changed.notify_all();
                return PublishOutcome::DisconnectedSlowClient;
            }
        }
        state.queue.push_back(event);
        self.inner.changed.notify_one();
        PublishOutcome::Queued
    }

    pub fn disconnect(&self) {
        let mut state = recover(self.inner.state.lock());
        state.disconnected = true;
        state.queue.clear();
        self.inner.changed.notify_all();
    }
}

impl EventReceiver {
    pub fn try_recv(&self) -> Option<Event> {
        recover(self.inner.state.lock()).queue.pop_front()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Option<Event> {
        let state = recover(self.inner.state.lock());
        let mut state = if state.queue.is_empty() && !state.disconnected {
            match self.inner.changed.wait_timeout(state, timeout) {
                Ok((guard, _)) => guard,
                Err(error) => error.into_inner().0,
            }
        } else {
            state
        };
        state.queue.pop_front()
    }

    pub fn is_disconnected(&self) -> bool {
        recover(self.inner.state.lock()).disconnected
    }
}

impl Drop for EventReceiver {
    fn drop(&mut self) {
        let mut state = recover(self.inner.state.lock());
        state.disconnected = true;
        state.queue.clear();
        self.inner.changed.notify_all();
    }
}

fn recover<'a, T>(
    result: Result<MutexGuard<'a, T>, PoisonError<MutexGuard<'a, T>>>,
) -> MutexGuard<'a, T> {
    match result {
        Ok(guard) => guard,
        Err(error) => error.into_inner(),
    }
}
