//! The part of fuxth that data shares without the interpreter: a failure's
//! code. `fuxth` gives it again; a host's wire and server use this crate,
//! and never the machine.
#![forbid(clippy::print_stderr)]
mod code;

pub use code::{Code, Failure};
