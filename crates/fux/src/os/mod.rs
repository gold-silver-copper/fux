//! Operating-system adapters: owned pane processes and PTY pumps. Blocking work lives here and on
//! dedicated threads, never inside ECS systems.

pub mod pty;

/// Locks a mutex, taking the data back from a poisoned lock: every guarded value here is a
/// counter or a queue whose partial update is harmless.
pub(crate) fn lock<T>(value: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
