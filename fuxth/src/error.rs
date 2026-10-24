//! Why a program could not be read or run: a stable code, what went wrong,
//! and where.
use crate::machine::Builtin;
use crate::token::{Place, Token};
use std::rc::Rc;

/// Expands `/// doc, Variant "name" [found], ...` to the enum `Code` of
/// those variants and `Host`; `name`, each variant's name (a host's code is
/// its own); and `raised`, whether a running word can raise it: not for a
/// row marked `[found]`, a mistake found before or after the words run, or
/// in the application.
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
            /// `try` may catch it.
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
    /// A string or list past the length or nesting allowed, or a run that
    /// built more than its share.
    TooLong "too_long",
    /// An application word gave more or fewer values than its vocabulary
    /// says: a mistake in the application, not the program.
    Contract "contract" [found],
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
    pub fn new(code: Code, message: impl Into<String>, at: Option<&Token>) -> Self {
        Self {
            code,
            message: message.into(),
            place: at.map(|t| t.place.clone()),
            word: at.map(|t| Rc::clone(&t.text)),
        }
    }

    /// Where it happened and why, as `Display` writes them after its code:
    /// `PLACE: WORD: MESSAGE`.
    pub fn located(&self) -> String {
        match &self.place {
            Some(place) => format!("{place}: {}", self.said()),
            None => self.said(),
        }
    }

    /// `WORD: MESSAGE`: what it says but where.
    fn said(&self) -> String {
        // An empty word (`define ''`) is said in its message, not here.
        match self.word.as_deref().filter(|w| !w.is_empty()) {
            Some(word) => format!("{word}: {}", self.message),
            None => self.message.clone(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.name(), self.located())
    }
}

impl std::error::Error for Error {}

/// What a step takes, as a value of another kind is told it: the same
/// words whether the checker finds it before the run or the machine during
/// it.
#[derive(Clone, Copy)]
pub(crate) enum Wants {
    /// What `if` and `until` take.
    Flag,
    /// What `each` walks.
    Each,
    /// A string or a list, for the word.
    Seq(Builtin),
    /// An integer, for the word.
    Int(Builtin),
    /// `lt`'s two values.
    Ordered,
    /// What `concat` joins.
    Concat,
    /// What is pushed onto a string.
    PushText,
    /// What is put into a string.
    PutText,
    /// What `code` takes.
    Char,
}

impl Wants {
    /// "each walks a string or a list, not an integer": `got`, the kinds
    /// found.
    /// `app` if what was found is a value of the application's alone.
    pub(crate) fn not(self, got: impl std::fmt::Display, app: bool) -> String {
        let said = match self {
            Self::Flag => return format!("takes a boolean (from eq or lt), not {got}"),
            Self::Int(word) => return format!("{} takes an integer, not {got}", word.name()),
            Self::Concat => {
                return format!("concat takes a list of strings or of lists, not {got}");
            }
            Self::Each => format!("each walks a string or a list, not {got}"),
            Self::Seq(word) => format!("{} takes a string or a list, not {got}", word.name()),
            Self::Ordered => {
                format!("lt compares two integers, two strings or two lists, not {got}")
            }
            Self::PushText => format!("push onto a string takes a string, not {got}"),
            Self::PutText => format!("put into a string takes a string, not {got}"),
            Self::Char => format!("code takes a one-character string, not {got}"),
        };
        // An application's value where text is taken: the word that makes
        // it text.
        match app {
            true => format!("{said}; `string` makes it text"),
            false => said,
        }
    }
}

/// A block whose body starts with an empty stack: nothing below it may be
/// taken there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Floor {
    List,
    Try,
}

/// What too few values says, the checker's and the machine's alike: "takes
/// 2 values, and the stack has 1". `takes` says how it takes them ("takes",
/// "needs at least"), `has` how many the stack has, `inside` the block it
/// is in, if one starts it empty.
pub(crate) fn underflow(takes: &str, n: usize, has: &str, inside: Option<Floor>) -> String {
    let values = if n == 1 { "value" } else { "values" };
    let inside = match inside {
        Some(Floor::List) => " (inside list … end, which starts with an empty stack)",
        Some(Floor::Try) => " (inside try … catch, which starts with an empty stack)",
        None => "",
    };
    format!("{takes} {n} {values}, and the stack has {has}{inside}")
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
