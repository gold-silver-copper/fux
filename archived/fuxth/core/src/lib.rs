//! The part of fuxth that data shares without the interpreter: a failure's
//! code, and how a word is quoted to be read back. `fuxth` gives them
//! again; a host's wire and server use this crate, and never the machine.
#![forbid(clippy::print_stderr)]
mod code;
mod quote;

pub use code::{Code, Failure};
pub use quote::{Quoted, quote};
