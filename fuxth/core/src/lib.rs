//! The part of fuxth that data shares without the interpreter: a failure's
//! code, the allocation grants that bound decoding and rendering, and how
//! long and deep a value may be. `fuxth` gives all of it again; a host's
//! wire and server use this crate, and never the machine.
#![forbid(clippy::print_stderr)]
mod code;
mod grant;

pub use code::{Code, Failure};
pub use grant::{Grant, Render};

/// The most elements one string or list a program builds may hold.
pub const LONGEST: usize = 1_000_000;

/// How deep lists may nest: a list of lists of … at most this many. Every
/// walk over a value (comparing, printing, dropping it) goes no deeper.
pub const NESTING: u8 = 64;
