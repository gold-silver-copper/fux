//! xterm itself, under Xvfb, read by printing its screen: not built yet; see the engine's entry in README.md.
use crate::engine::{Can, Engine, Kind, Setup};

pub const KIND: Kind = Kind {
    name: "xterm",
    about: "xterm itself, under Xvfb, read by printing its screen",
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
    Err("xterm: not built yet".into())
}
