//! asciinema's virtual terminal, avt: not built yet; see the engine's entry in README.md.
use crate::engine::{Can, Engine, Kind, Setup};

pub const KIND: Kind = Kind {
    name: "avt",
    about: "asciinema's virtual terminal, avt",
    can: Can::ALL,
    panel: true,
    in_process: true,
    available,
    make,
};

fn available() -> Result<(), String> {
    Err("not built yet".into())
}

fn make(_: &Setup) -> Result<Box<dyn Engine>, String> {
    Err("avt: not built yet".into())
}
