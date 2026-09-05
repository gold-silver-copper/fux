//! Multiplexer-owned terminal emulation. Adapted from koh; see LICENSES/koh.txt.
mod server;
pub use server::{Progress, ServerTerminal, UNHANDLED_OSC_MAX_LEN, UNHANDLED_OSC_RING};

pub const DEFAULT_ROWS: u16 = 24;
pub const DEFAULT_COLS: u16 = 80;
pub const MIN_DIM: u16 = 2;
pub const MAX_DIM: u16 = 1000;
pub const MAXIMUM_CLIPBOARD_SIZE: usize = 16 * 1024;
const MAX_TITLE_LEN: usize = 256;

pub fn clamp_dims(rows: u16, cols: u16) -> (u16, u16) {
    (rows.clamp(MIN_DIM, MAX_DIM), cols.clamp(MIN_DIM, MAX_DIM))
}

fn process_contained<C: vt100::Callbacks>(parser: &mut vt100::Parser<C>, bytes: &[u8]) -> bool {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parser.process(bytes))).is_ok()
}

/// A pane snapshot, independent of any network synchronization protocol.
#[derive(Clone)]
pub struct TerminalScreen {
    screen: vt100::Screen,
    title: String,
    icon: String,
    clipboard: String,
    bell_count: u64,
    exit_code: Option<u32>,
}
impl TerminalScreen {
    pub fn screen(&self) -> &vt100::Screen {
        &self.screen
    }
    pub fn title(&self) -> &str {
        &self.title
    }
    pub fn icon(&self) -> &str {
        &self.icon
    }
    pub fn clipboard(&self) -> &str {
        &self.clipboard
    }
    pub fn bell_count(&self) -> u64 {
        self.bell_count
    }
    pub fn exit_code(&self) -> Option<u32> {
        self.exit_code
    }
}
