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
