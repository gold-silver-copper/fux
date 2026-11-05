//! Terminals: on Unix their modes, their size, their foreground group, and
//! which session they control (`unix.rs`); on Windows a pseudo console's
//! size (`windows.rs`). A [`Size`] and [`Pixels`] are the same on both.
use std::num::NonZeroU16;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// A terminal's size: never no rows or no columns, which a program would
/// divide by, or draw nothing in.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Size {
    pub rows: NonZeroU16,
    pub cols: NonZeroU16,
}

impl Size {
    /// `rows` by `cols`, if neither is zero: a size read off the wire or a
    /// command line, or a default such as 24 by 80.
    pub fn new(rows: u16, cols: u16) -> Option<Size> {
        Some(Size::from((NonZeroU16::new(rows)?, NonZeroU16::new(cols)?)))
    }
}

impl From<(NonZeroU16, NonZeroU16)> for Size {
    fn from((rows, cols): (NonZeroU16, NonZeroU16)) -> Size {
        Size { rows, cols }
    }
}

/// The size of a terminal's whole window in pixels, as `TIOCSWINSZ` takes
/// it: for a program that lays pictures out in it, or divides it by a
/// size in cells for its cell.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Pixels {
    pub width: NonZeroU16,
    pub height: NonZeroU16,
}
