//! Alacritty's terminal core, alacritty_terminal: not built yet; see the engine's entry in README.md.
use crate::engine::{Can, Engine, Kind, Setup};

pub const KIND: Kind = Kind {
    name: "alacritty",
    about: "Alacritty's terminal core, alacritty_terminal",
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
    Err("alacritty: not built yet".into())
}
