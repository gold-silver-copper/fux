//! One file per engine. Each makes an [`Engine`](crate::engine::Engine)
//! and says what it can be asked in its `KIND`. Built without the
//! `engines` feature, the engines it links stand in [`absent`]'s places.
#[cfg(not(feature = "engines"))]
mod absent;
#[cfg(not(feature = "engines"))]
pub use absent::{alacritty, avt, ghostty, libvterm, vt100, wezterm};
#[cfg(feature = "engines")]
pub mod alacritty;
#[cfg(feature = "engines")]
pub mod avt;
pub mod fux_vt;
#[cfg(feature = "engines")]
pub mod ghostty;
#[cfg(feature = "engines")]
pub mod libvterm;
pub mod pane;
pub mod tmux;
#[cfg(feature = "engines")]
pub mod vt100;
#[cfg(feature = "engines")]
pub mod wezterm;
pub mod xterm;
pub mod xterm_js;
