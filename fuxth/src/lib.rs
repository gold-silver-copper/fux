//! fuxth: a small Forth for the shell.
//!
//! A program is words. Numbers push themselves; `str` pushes the next word
//! as a string; every other word takes values from the stack and gives
//! values back, and never reads the words after it. The parsing words are
//! few and fixed: `str`, `define NAME … end`, `let NAME`, `list … end`,
//! `each … end`, `if … else … then` and `begin … until`. Words have names,
//! not symbols (`add`, `eq`, `dup`), and none uses a character a shell
//! treats specially, so a program can be a shell command's arguments as
//! well as a file.
//!
//! The core is small (21 words: `drop pick roll`, arithmetic, `lt eq`,
//! sequences, characters, `kind string print fail`); everything else is
//! the standard library ([`STDLIB`]), written in fuxth.
//!
//! ```text
//! define squares let n list 1 n 1 add range each dup mul end end end
//! 4 squares print          # prints 1 4 9 16
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
pub use machine::{Args, BUILT, Done, Host, LONGEST, Limits, Machine, Outcome};
pub use token::{Place, Token};
pub use value::{AppValue, List, NESTING, TOTAL, Text, Value};

/// fuxth's standard library, written in fuxth: every word beyond the core
/// (`dup swap over rot`, booleans, comparing, sequences, text …). An
/// application compiles it before its own words' library and the program.
pub const STDLIB: &str = include_str!("stdlib.fux");

#[cfg(test)]
mod tests;
