//! Why a program could not be read or run: a stable code, what went wrong,
//! and where.
use crate::token::Place;
use std::rc::Rc;

/// What kind of failure: each has a name that stays the same, for a script
/// to tell failures apart by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    /// A quote is never closed.
    OpenQuote,
    /// A word no definition, builtin or application gives.
    UnknownWord,
    /// A parsing word at the end of the program, with nothing to read.
    MissingToken,
    /// A name for `define` or `let` that is taken, a number, or a parsing
    /// word.
    BadName,
    /// `define` inside a definition or a block.
    NotTopLevel,
    /// `end`, `else`, `then` or `until` with nothing open for it.
    Unmatched,
    /// A definition or block still open at the end.
    Unclosed,
    /// A word given fewer values than it takes.
    StackUnderflow,
    /// The stack grew past its limit.
    StackOverflow,
    /// Calls nested past their limit: recursion too deep.
    CallDepth,
    /// A value of the wrong type for a word.
    Type,
    /// Division or remainder by zero.
    DivByZero,
    /// Arithmetic past what an integer holds.
    Overflow,
    /// A `let` name read before its `let` ran.
    Unset,
    /// An index or slice past a string's or list's end, or a number out of
    /// a word's range (a character code, a negative depth).
    Range,
    /// `fail`: the program stopped itself, saying why.
    Fail,
    /// A string or list past the length or nesting allowed, or a run that
    /// built more than its share.
    TooLong,
    /// An application word gave more or fewer values than its vocabulary
    /// says: a mistake in the application, not the program.
    Contract,
    /// The application's own failure, its code given.
    Host(&'static str),
}

impl Code {
    /// The code's name.
    pub fn name(self) -> &'static str {
        match self {
            Code::OpenQuote => "open_quote",
            Code::UnknownWord => "unknown_word",
            Code::MissingToken => "missing_token",
            Code::BadName => "bad_name",
            Code::NotTopLevel => "not_top_level",
            Code::Unmatched => "unmatched",
            Code::Unclosed => "unclosed",
            Code::StackUnderflow => "stack_underflow",
            Code::StackOverflow => "stack_overflow",
            Code::CallDepth => "call_depth",
            Code::Type => "type",
            Code::DivByZero => "div_by_zero",
            Code::Overflow => "overflow",
            Code::Unset => "unset",
            Code::Range => "range",
            Code::Fail => "fail",
            Code::TooLong => "too_long",
            Code::Contract => "contract",
            Code::Host(code) => code,
        }
    }
}

/// A failure, with where it happened: the token's place and text, when
/// there is one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub code: Code,
    pub message: String,
    pub place: Option<Place>,
    pub word: Option<Rc<str>>,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: ", self.code.name())?;
        // An empty word (`define ''`) is said in its message, not here.
        let word = self.word.as_deref().filter(|w| !w.is_empty());
        match (&self.place, word) {
            (Some(place), Some(word)) => write!(f, "{place}: {word}: ")?,
            (Some(place), None) => write!(f, "{place}: ")?,
            (None, Some(word)) => write!(f, "{word}: ")?,
            (None, None) => {}
        }
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// What an application word gives when it fails: its code and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: &'static str,
    pub message: String,
}

impl Failure {
    pub fn new(code: &'static str, message: impl Into<String>) -> Failure {
        Failure {
            code,
            message: message.into(),
        }
    }
}
