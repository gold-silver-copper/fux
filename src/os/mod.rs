//! Operating-system adapters: owned pane processes and PTY pumps. Blocking work lives here and on
//! dedicated threads, never inside ECS systems.

pub mod pty;
