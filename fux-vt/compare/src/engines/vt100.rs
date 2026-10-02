//! the vt100 crate, fux-vt's ancestor: a speed baseline, not a voter: not built yet; see the engine's entry in README.md.
use crate::engine::{Can, Engine, Kind, Setup};

pub const KIND: Kind = Kind {
    name: "vt100",
    about: "the vt100 crate, fux-vt's ancestor: a speed baseline, not a voter",
    can: Can::ALL,
    panel: false,
    in_process: true,
    available,
    make,
};

fn available() -> Result<(), String> {
    Err("not built yet".into())
}

fn make(_: &Setup) -> Result<Box<dyn Engine>, String> {
    Err("vt100: not built yet".into())
}
