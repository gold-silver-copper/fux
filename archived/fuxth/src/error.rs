//! Why a program could not be read or run: a stable code, what went wrong,
//! and where.
use crate::Code;
use crate::machine::Builtin;
use crate::token::{Place, Token};
use std::rc::Rc;

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

/// What a step takes, as a value of another kind is told it.
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
    /// An integer or an address, for the word.
    Number(Builtin),
    /// `lt`'s two values.
    Ordered,
    /// What `concat` joins.
    Concat,
    /// What is pushed onto a string.
    PushText,
    /// What is put into a string.
    PutText,
    /// What the word takes, said.
    Takes(Builtin, &'static str),
}

impl Wants {
    /// "each walks a string or a list, not an integer": `got`, the kinds
    /// found.
    /// `app` if what was found is a value of the application's alone.
    pub(crate) fn not(self, got: impl std::fmt::Display, app: bool) -> String {
        let said = match self {
            Self::Flag => {
                return format!("takes a boolean (from eq or lt) or an integer, not {got}");
            }
            Self::Int(word) => return format!("{} takes an integer, not {got}", word.name()),
            Self::Number(word) => {
                return format!("{} takes an integer or an address, not {got}", word.name());
            }
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
            Self::Takes(word, what) => format!("{} takes {what}, not {got}", word.name()),
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

/// What too few values says: "takes 2 values, and the stack has 1".
/// `has` is how many the stack has, `inside` the block it is in, if one
/// starts it empty.
pub(crate) fn underflow(n: usize, has: usize, inside: Option<Floor>) -> String {
    let values = if n == 1 { "value" } else { "values" };
    let inside = match inside {
        Some(Floor::List) => " (inside list … end, which starts with an empty stack)",
        Some(Floor::Try) => " (inside try … catch, which starts with an empty stack)",
        None => "",
    };
    format!("takes {n} {values}, and the stack has {has}{inside}")
}
