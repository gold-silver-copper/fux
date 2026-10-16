//! Why a program could not be read or run: a stable code, what went wrong,
//! and where.
use crate::token::{Place, Token};
use std::rc::Rc;

/// Expands `/// doc, Variant "name", ...` to the enum `Code` of those
/// variants and `Host`, and `name`, each variant's name (a host's code is
/// its own).
macro_rules! codes {
    ($($(#[$doc:meta])* $variant:ident $name:literal,)*) => {
        /// What kind of failure: each has a name that stays the same, for a
        /// script to tell failures apart by.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Code {
            $($(#[$doc])* $variant,)*
            /// The application's own failure, its code given.
            Host(&'static str),
        }

        impl Code {
            /// The code's name.
            pub fn name(self) -> &'static str {
                match self {
                    $(Code::$variant => $name,)*
                    Code::Host(code) => code,
                }
            }
        }
    };
}

codes! {
    /// A quote is never closed.
    OpenQuote "open_quote",
    /// A word no definition, builtin or application gives.
    UnknownWord "unknown_word",
    /// A parsing word at the end of the program, with nothing to read.
    MissingToken "missing_token",
    /// A name for `define` or `let` that is taken, a number, or a parsing
    /// word.
    BadName "bad_name",
    /// `define` inside a definition or a block.
    NotTopLevel "not_top_level",
    /// `end`, `else`, `then` or `until` with nothing open for it.
    Unmatched "unmatched",
    /// A definition or block still open at the end.
    Unclosed "unclosed",
    /// A word given fewer values than it takes.
    StackUnderflow "stack_underflow",
    /// The stack grew past its limit.
    StackOverflow "stack_overflow",
    /// Calls nested past their limit: recursion too deep.
    CallDepth "call_depth",
    /// A value of the wrong type for a word.
    Type "type",
    /// Division or remainder by zero.
    DivByZero "div_by_zero",
    /// Arithmetic past what an integer holds.
    Overflow "overflow",
    /// A `let` name read before its `let` ran.
    Unset "unset",
    /// An index or slice past a string's or list's end, or a number out of
    /// a word's range (a character code, a negative depth).
    Range "range",
    /// `fail`: the program stopped itself, saying why.
    Fail "fail",
    /// A string or list past the length or nesting allowed, or a run that
    /// built more than its share.
    TooLong "too_long",
    /// An application word gave more or fewer values than its vocabulary
    /// says: a mistake in the application, not the program.
    Contract "contract",
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

impl Error {
    /// A failure at `at`, the token it names, when there is one.
    pub fn new(code: Code, message: impl Into<String>, at: Option<&Token>) -> Error {
        Error {
            code,
            message: message.into(),
            place: at.map(|t| t.place.clone()),
            word: at.map(|t| Rc::clone(&t.text)),
        }
    }
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
