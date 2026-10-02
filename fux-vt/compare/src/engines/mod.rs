//! One file per engine. Each makes an [`Engine`](crate::engine::Engine)
//! and says what it can be asked in its `KIND`.
pub mod alacritty;
pub mod avt;
pub mod fux_vt;
pub mod ghostty;
pub mod libvterm;
pub mod pane;
pub mod tmux;
pub mod vt100;
pub mod wezterm;
pub mod xterm;
pub mod xterm_js;
