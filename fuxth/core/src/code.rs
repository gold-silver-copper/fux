//! What kind of failure: the codes a program's failures have, and what an
//! application word gives when it fails.
use std::rc::Rc;

/// Expands `/// doc, Variant "name" [found], ...` to the enum `Code` of
/// those variants and `Host`; `name`, each variant's name (a host's code is
/// its own); and `raised`, whether a running word of fuxth's can raise it:
/// not for a row marked `[found]`, a mistake found before or after the
/// words run, or in the application.
macro_rules! codes {
    (@raised) => {
        true
    };
    (@raised found) => {
        false
    };
    ($($(#[$doc:meta])* $variant:ident $name:literal $([$found:ident])?,)*) => {
        /// What kind of failure: each has a name that stays the same, for a
        /// script to tell failures apart by.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Code {
            $($(#[$doc])* $variant,)*
            /// The application's own failure, its code given.
            Host(&'static str),
        }

        impl Code {
            /// Every code but a host's, in order.
            pub const ALL: &[Code] = &[$(Code::$variant),*];

            /// The code's name.
            pub fn name(self) -> &'static str {
                match self {
                    $(Code::$variant => $name,)*
                    Code::Host(code) => code,
                }
            }

            /// Whether a word can raise it as the program runs, so that a
            /// `try` may catch it: in plain fuxth; a host may declare that
            /// its words raise more (`Vocabulary::raises`).
            pub fn raised(self) -> bool {
                match self {
                    $(Code::$variant => codes!(@raised $($found)?),)*
                    Code::Host(_) => true,
                }
            }
        }
    };
}

codes! {
    /// A source cannot be required, or its imports form a cycle.
    Require "require" [found],
    /// A quote is never closed.
    OpenQuote "open_quote" [found],
    /// A word no definition, builtin or application gives.
    UnknownWord "unknown_word" [found],
    /// A parsing word at the end of the program, with nothing to read.
    MissingToken "missing_token" [found],
    /// A name for `define` or `let` that is taken, a number, or a parsing
    /// word.
    BadName "bad_name" [found],
    /// `define` inside a definition or a block.
    NotTopLevel "not_top_level" [found],
    /// `end`, `else`, `then` or `until` with nothing open for it.
    Unmatched "unmatched" [found],
    /// A definition or block still open at the end.
    Unclosed "unclosed" [found],
    /// A word given fewer values than it takes.
    StackUnderflow "stack_underflow",
    /// A value of the wrong type for a word.
    Type "type",
    /// Division or remainder by zero.
    DivByZero "div_by_zero",
    /// Arithmetic past what an integer holds.
    Overflow "overflow",
    /// A `let` name read before its `let` ran, or one a definition reads
    /// where it is not set (a key binding's, of its config's).
    Unset "unset",
    /// A value of a kind the application says must be used, left at the
    /// program's end.
    Unused "unused" [found],
    /// An index or slice past a string's or list's end, or a number out of
    /// a word's range (a character code, a negative depth).
    Range "range",
    /// `fail`: the program stopped itself, saying why.
    Fail "fail",
    /// A `try`'s codes not a string written just before it, or naming a
    /// code no word raises.
    BadCodes "bad_codes" [found],
    /// A value nested deeper than its application carries it (its wire).
    TooLong "too_long",
    /// An application word gave more or fewer values than its vocabulary
    /// says: a mistake in the application, not the program.
    Contract "contract" [found],
}

/// What an application word gives when it fails: its code (the language's
/// own, when it is one of them, or `Code::Host`) and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: Code,
    pub message: String,
    /// The word to name where it failed, if not the application's word:
    /// one in a program the word was given (a key binding's).
    pub word: Option<Rc<str>>,
}

impl Failure {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            word: None,
        }
    }
}
