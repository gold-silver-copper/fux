//! tmux, a server of its own, read with capture-pane: not built yet; see the engine's entry in README.md.
use crate::engine::{Can, Engine, Kind, Setup};

pub const KIND: Kind = Kind {
    name: "tmux",
    about: "tmux, a server of its own, read with capture-pane",
    can: Can::ALL,
    panel: false,
    in_process: false,
    available,
    make,
};

fn available() -> Result<(), String> {
    Err("not built yet".into())
}

fn make(_: &Setup) -> Result<Box<dyn Engine>, String> {
    Err("tmux: not built yet".into())
}
