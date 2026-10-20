//! The engines this process links, when it is built without them
//! (`--no-default-features`): each keeps its name and its place in the
//! panel, cannot run here, and makes nothing.
use crate::engine::{Blanks, Can, Engine, Kind, Setup};

const WHY: &str = "fux-vt-compare was built without its engines feature";

fn absent() -> Result<(), String> {
    Err(WHY.to_owned())
}

fn none(_: &Setup) -> Result<Box<dyn Engine>, String> {
    Err(WHY.to_owned())
}

const fn kind(name: &'static str, panel: bool) -> Kind {
    Kind {
        name,
        about: "not built",
        can: Can::NO_DEVICE,
        blanks: Blanks::XTERM,
        panel,
        in_process: true,
        available: absent,
        make: none,
    }
}

macro_rules! with_history {
    ($($name:ident $panel:literal),*) => {$(
        pub mod $name {
            use super::{Engine, Setup, WHY};
            pub const KIND: super::Kind = super::kind(stringify!($name), $panel);
            pub fn with_history(_: &Setup, _: usize) -> Result<Box<dyn Engine>, String> {
                Err(WHY.to_owned())
            }
        }
    )*};
}
with_history!(alacritty true, avt true, libvterm true, vt100 false, wezterm true);

pub mod ghostty {
    use super::{Engine, Setup, WHY};
    pub const KIND: super::Kind = super::kind("ghostty", true);
    pub fn with_scrollback(_: &Setup, _: usize) -> Result<Box<dyn Engine>, String> {
        Err(WHY.to_owned())
    }
    pub fn answering(_: u16, _: u16) -> Result<Box<dyn Engine>, String> {
        Err(WHY.to_owned())
    }
}
