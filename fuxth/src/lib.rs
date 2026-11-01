//! fuxth: a small Forth for the shell.
//!
//! A program is words, run as they are read. Numbers push themselves;
//! `str` pushes the next word as a string; most words take values from the
//! stack and give values back. The words that shape a program are
//! immediate words, which run even while a definition is compiled:
//! `define NAME … end`, `str`, `let NAME`, `list … end`, `each … end`,
//! `if … else … then`, `begin … until`, `try … catch … end`, `code … end`
//! and `require PATH` among them; a program may define more, with
//! `immediate` and `postpone`. Words have names, not symbols (`add`, `eq`,
//! `dup`), and none uses a character a shell treats specially, so a
//! program can be a shell command's arguments as well as a file.
//!
//! The core is small (arithmetic, `lt eq`, sequences, the dictionary's
//! words and the immediate ones); everything else is the standard library
//! ([`STDLIB`]), written in fuxth.
//!
//! ```text
//! define squares let n list 1 n 1 add range each dup mul end end end
//! 4 squares print          # prints 1 4 9 16
//! ```
//!
//! An application embeds it: it gives a [`Vocabulary`] of its own words,
//! each with its stack effect; it gives a [`Machine`] the program's words
//! to read ([`Machine::read`]) and runs it ([`Machine::run`]) as its
//! [`Host`]. A word that needs the application to do something first (ask
//! a server, say) suspends the run, and the application runs it again once
//! it has the answer, or raises its failure there ([`Machine::raise`]).
//! Nothing blocks, nothing runs on its own thread, and the whole state of a
//! run is data, so dropping it stops it.
//!
//! A word is found as it is read: one no one defined yet is an error then,
//! after the words before it acted. A definition is found from its `end`
//! on, and code compiled before keeps the version it was compiled against.
//! Each word checks its own values as it runs. Every error has a stable
//! code, a message, and the file, line and word it was at.
//!
//! A call that is a definition's last step replaces its caller's frame, so
//! recursion in last place is a loop.
#![forbid(clippy::print_stderr)]
mod error;
mod machine;
mod reader;
pub mod token;
mod value;
mod vocabulary;

pub use error::Error;
pub use fuxth_core::{Code, Failure};
pub use machine::{Args, Builtin, Done, Host, Machine, Outcome};
pub use reader::Doc;
pub use token::{Place, Token};
pub use value::{AppValue, List, Mark, Text, Value, Xt};
pub use vocabulary::Vocabulary;

/// fuxth's standard library, written in fuxth: every word beyond the core
/// (`dup swap over rot`, booleans, comparing, sequences, text …). An
/// application reads it before its own words' library and the program.
pub const STDLIB: &str = include_str!("stdlib.fux");

#[cfg(test)]
mod tests;
