//! libvterm, the engine of Neovim's and Vim's terminals, compiled from its release source: not built yet; see the engine's entry in README.md.
use crate::engine::{Can, Engine, Kind, Setup};

pub const KIND: Kind = Kind {
    name: "libvterm",
    about: "libvterm, the engine of Neovim's and Vim's terminals, compiled from its release source",
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
    Err("libvterm: not built yet".into())
}
