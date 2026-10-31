//! The part of fuxth that data shares without the interpreter: a failure's
//! code, and how deep a value may be. `fuxth` gives all of it again; a
//! host's wire and server use this crate, and never the machine.
#![forbid(clippy::print_stderr)]
mod code;

pub use code::{Code, Failure};

/// How deep lists may nest: a list of lists of … at most this many. Every
/// walk over a value (comparing, printing, dropping it) is a recursion that
/// goes no deeper, so none can overflow the stack.
pub const NESTING: u8 = 64;
