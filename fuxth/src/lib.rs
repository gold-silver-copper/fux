//! fuxth: a small Forth for the shell.
//!
//! A program is words. Numbers push themselves; `str` pushes the next word
//! as a string; every other word takes values from the stack and gives
//! values back, and never reads the words after it. The parsing words are
//! few and fixed: `str`, `define NAME … end`, `constant NAME`, and control
//! flow, `if … else … then` and `begin … until`. Built-in words have names,
//! not symbols (`add`, `eq`, `dup`), and none uses a character a shell
//! treats specially, so a program can be a shell command's arguments as
//! well as a file.
//!
//! ```text
//! define countdown
//!   begin dup 1 sub dup 0 eq until
//! end
//! 3 countdown          # leaves 3 2 1 0
//! ```
//!
//! An application embeds it: it gives a [`Vocabulary`] of its own words,
//! each with how many values it takes and gives, and its own kind of value;
//! it compiles a program ([`Compiler`]) and runs it ([`Machine::run`]) as its
//! [`Host`]. A word that needs the application to do something first (ask
//! a server, say) suspends the run, and the application runs it again once
//! it has the answer. Nothing blocks, nothing runs on its own thread, and
//! the whole state of a run is data, so dropping it stops it.
//!
//! The whole program is compiled before any of it runs: a word no one
//! defines, a block left open or a stray `end` is an error before the
//! program does anything, and so is a step that every run reaching it would
//! fail, for too few values or a value of the wrong kind. Every error has a
//! stable code, a message, and the file, line and word it was at.
//!
//! A call that is a definition's last step replaces its caller's frame, so
//! recursion in last place is a loop.
mod check;
mod compile;
mod error;
mod machine;
pub mod token;
mod value;

pub use compile::{Builtin, Compiler, PARSING, Program, Vocabulary};
pub use error::{Code, Error, Failure};
pub use machine::{Args, Done, Host, Limits, Machine, Outcome};
pub use token::{Place, Token};
pub use value::{AppValue, Value};

/// Words written in fuxth for every program: `true false nip tuck -rot
/// 2dup 2drop negate abs min max ne le ge`. An application compiles it
/// before its own words' library and the program.
pub const PRELUDE: &str = include_str!("prelude.fux");

#[cfg(test)]
mod tests;
