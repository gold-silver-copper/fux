//! fux's pane process: a pane's terminal, its input queue and its
//! program (`host`, `pane`), the grid its screen is drawn on (`render`),
//! its captures, and what it and its server say to each other (`link`).
//! fux-server holds the same code for a session kept in one process.

/// Writes to the log, the process's stderr: `log!("…", args)` writes the
/// line after `fux server: `. A log that cannot be written, a full disk,
/// say, loses the line and stops nothing.
#[doc(hidden)]
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        let _ = std::io::Write::write_fmt(
            &mut std::io::stderr(),
            format_args!("fux server: {}\n", format_args!($($arg)*)),
        );
    }};
}

/// `wired!(Ty { Unit, Tuple(a, b), Named { x, y } })` puts an enum of the
/// server's on the wire, and `wired!(struct Ty { a, b })` a struct's fields
/// in order: fux-wire's own `wire_enum!` and `wire_struct!`, by one name.
/// For what fux's own processes say to each other (`link`), never to a
/// client.
#[doc(hidden)]
#[macro_export]
macro_rules! wired {
    (struct $ty:path { $($f:ident),+ $(,)? }) => {
        $crate::fux_wire::wire_struct!($ty { $($f),+ });
    };
    ($($enumeration:tt)*) => {
        $crate::fux_wire::wire_enum!($($enumeration)*);
    };
}

#[doc(hidden)]
pub use fux_wire;

pub mod capture;
pub mod host;
pub mod link;
pub mod pane;
pub mod process;
pub mod render;
pub mod socket;
pub mod told;

use std::time::{Duration, Instant};

/// `wait` after `from`. A time too far off for an `Instant` is taken as
/// `from`, so that a deadline fires at once rather than never.
pub fn after(from: Instant, wait: Duration) -> Instant {
    from.checked_add(wait).unwrap_or(from)
}

/// How the tests feed a parser and read its screen, as fux-vt's own do.
#[cfg(test)]
#[path = "../../fux-vt/tests/corpus/feed.rs"]
#[expect(dead_code, reason = "the tests use some of the helpers")]
mod vt_feed;
