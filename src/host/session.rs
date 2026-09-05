//! Multiplexer host lifecycle and viewer notifications. No transport or identity types.
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId(u64);
impl ClientId {
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}
#[derive(Clone, Debug)]
pub struct ChangeSignal(watch::Sender<u64>);
impl Default for ChangeSignal {
    fn default() -> Self {
        Self(watch::Sender::new(0))
    }
}
impl ChangeSignal {
    pub fn pulse(&self) {
        self.0.send_modify(|value| *value = value.wrapping_add(1));
    }
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.0.subscribe()
    }
}
pub trait SessionHost: Send + 'static {
    type State: Send + 'static;
    fn snapshot(&mut self) -> Self::State;
    fn input(&mut self, bytes: &[u8]);
    fn pane_input(&mut self, bytes: &[u8]) {
        self.input(bytes);
    }
    fn application_mouse(&mut self, _event: crate::host::MouseEvent) {}
    fn external_binding(&mut self, _key: u8) -> bool {
        false
    }
    fn control(&mut self, _request: crate::control::Request) -> Option<crate::control::Reply> {
        None
    }
    /// Read one viewer's scrollback window without changing shared viewport or selection state.
    fn copy_view(&mut self, _pane: u32, _offset: u32) -> Option<crate::state::PaneView> {
        None
    }
    fn resize(&mut self, client: ClientId, rows: u16, columns: u16);
    fn alive(&self) -> bool;
    fn attach_notify(&mut self, _changed: ChangeSignal) {}
    fn client_detached(&mut self, _client: ClientId) {}
    fn kill(&mut self) {}
    fn shutdown(self)
    where
        Self: Sized,
    {
    }
}
