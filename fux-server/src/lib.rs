//! fux's server: one thread holds workspaces, tabs and split panes, their
//! programs, and the clients attached to them, and answers each request
//! in `fux-wire`'s terms. It never runs fuxth (D-2): programs run in
//! clients.

/// Writes to the server's log, its stderr: `log!("…", args)` writes the
/// line after `fux server: `. A log that cannot be written, a full disk,
/// say, loses the line and stops nothing.
macro_rules! log {
    ($($arg:tt)*) => {{
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!("fux server: {}\n", format_args!($($arg)*)),
        );
    }};
}

pub mod capture;
mod input;
pub mod journal;
pub mod outer;
pub mod pane;
pub mod process;
pub mod render;
pub mod server;
pub mod session;
pub mod socket;
pub mod view;
pub mod wait;
pub mod workspace;

use fuxix::process::Signal;
use std::io::{PipeReader, Read};
use std::time::{Duration, Instant};

/// `wait` after `from`. A time too far off for an `Instant` is taken as
/// `from`, so that a deadline fires at once rather than never.
pub(crate) fn after(from: Instant, wait: Duration) -> Instant {
    from.checked_add(wait).unwrap_or(from)
}

/// The signals that stop a server and detach a client.
pub const STOPS: [Signal; 3] = [Signal::Term, Signal::Int, Signal::Hup];

/// Reads whatever a pipe holds.
pub fn drain(mut pipe: &PipeReader) {
    let mut buffer = [0u8; 256];
    while matches!(pipe.read(&mut buffer), Ok(n) if n > 0) {}
}

/// How the tests feed a parser and read its screen, as fux-vt's own do.
#[cfg(test)]
#[path = "../../fux-vt/tests/corpus/feed.rs"]
#[expect(dead_code, reason = "the tests use some of the helpers")]
mod vt_feed;

/// Pictures for the tests.
#[cfg(test)]
mod test_picture {
    /// A picture for the tests, `width` by `height` pixels of one colour, each
    /// channel off or full, drawn as a program draws one: sixel, read by
    /// fux-vt.
    pub(crate) fn solid(
        width: u32,
        height: u32,
        [r, g, b]: [bool; 3],
    ) -> Result<std::sync::Arc<fux_vt::sixel::Picture>, String> {
        use crate::vt_feed::Feed;
        let options = fux_vt::Options::from(fux_vt::Feature::Pictures);
        let size = fux_vt::Size::new(24, 80).map_err(|e| e.to_string())?;
        let mut parser =
            fux_vt::Parser::with_options(size, 0, options).map_err(|e| e.to_string())?;
        let cell = fux_vt::CellSize::new(10, 20).ok_or("a cell size")?;
        parser.set_host_graphics(fux_vt::Graphics::Sixel(cell));
        let full = |on| if on { 100 } else { 0 };
        let bands = usize::try_from(height.div_ceil(6)).map_err(|e| e.to_string())?;
        let bands = vec![format!("!{width}~"); bands].join("-");
        let (r, g, b) = (full(r), full(g), full(b));
        let sixel = format!("\x1bP0;1q\"1;1;{width};{height}#1;2;{r};{g};{b}#1{bands}\x1b\\");
        parser
            .process(sixel.as_bytes())
            .map_err(|e| e.to_string())?;
        let shown = parser.screen().pictures().next().ok_or("no picture")?;
        Ok(std::sync::Arc::clone(shown.picture))
    }
}
