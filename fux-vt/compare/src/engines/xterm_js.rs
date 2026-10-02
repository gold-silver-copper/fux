//! xterm.js, as @xterm/headless under Node: not built yet; see the engine's entry in README.md.
use crate::engine::{Can, Engine, Kind, Setup};

pub const KIND: Kind = Kind {
    name: "xterm.js",
    about: "xterm.js, as @xterm/headless under Node",
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
    Err("xterm.js: not built yet".into())
}
