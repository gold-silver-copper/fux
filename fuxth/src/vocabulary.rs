//! The application's words: each with its stack effect (`pane text --
//! answer`), whose names before the `--` say how many values it takes; the
//! codes its words fail with; and the kinds of its values that must be used.
use crate::Code;
use crate::error::Error;
use crate::value::{AppValue, Value};

/// A word: its name, and how many values it takes.
#[derive(Clone, Debug)]
struct Word {
    name: String,
    takes: usize,
}

/// The application's words, the codes of its failures, and the kinds of
/// its values that must be used, each with what one left over is told.
/// Each name means one thing: a second word called as another word is (the
/// core's and the parsing words among them), or a code declared twice, is
/// refused, never kept beside the first or in its place.
#[derive(Clone, Debug, Default)]
pub struct Vocabulary {
    words: Vec<Word>,
    unused: Vec<(String, String)>,
    codes: Vec<Code>,
}

fn refused(message: String) -> Error {
    Error::new(Code::Contract, message, None)
}

impl Vocabulary {
    /// Values of kind `name` (as its values' `AppValue::name` says it) must
    /// be used: one left on the stack at a program's end is an error,
    /// saying `why`.
    pub fn must_use(&mut self, name: &str, why: &str) -> Result<(), Error> {
        if self.unused.iter().any(|(n, _)| n == name) {
            return Err(refused(format!("{name}: must be used already")));
        }
        self.unused.push((name.to_owned(), why.to_owned()));
        Ok(())
    }

    /// Adds a word, `effect` saying what it takes and gives, `--` between
    /// (`pane text -- answer`): it is given as many values as there are
    /// names before the `--`. Its id, which the host is called with, is the
    /// number of words added before it. A word is added once, under a name
    /// none of the core's or parsing words has: one name, one word.
    pub fn add(&mut self, name: &str, effect: &str) -> Result<usize, Error> {
        let held = if self.words.iter().any(|w| w.name == name) {
            Some("a word already")
        } else if crate::Builtin::named(name).is_some() {
            Some("a word of the core")
        } else if crate::Parsing::named(name).is_some() {
            Some("a word of the language")
        } else {
            None
        };
        if let Some(held) = held {
            return Err(refused(format!("{name}: {held}")));
        }
        let Some((before, _)) = effect.split_once("--") else {
            return Err(refused(format!("{name}: no -- in {effect:?}")));
        };
        self.words.push(Word {
            name: name.to_owned(),
            takes: before.split_whitespace().count(),
        });
        Ok(self.words.len().saturating_sub(1))
    }

    /// A code the application's words may fail with, which a `try` may
    /// name: one of its own (`Code::Host`), a word that is none of the
    /// language's; or one of the language's that plain fuxth finds only
    /// compiling or at a program's end (`Code::raised` is false), which its
    /// words raise as they run. One fuxth's words raise already changes
    /// nothing; one declared twice is refused.
    pub fn raises(&mut self, code: Code) -> Result<(), Error> {
        let name = code.name();
        if self.codes.contains(&code) {
            return Err(refused(format!("{name}: declared already")));
        }
        if let Code::Host(_) = code {
            if name.is_empty() || name.contains(char::is_whitespace) {
                return Err(refused(format!("{name:?}: a code is one word")));
            }
            if Code::ALL.iter().any(|c| c.name() == name) {
                return Err(refused(format!("{name}: a code of the language's")));
            }
        }
        self.codes.push(code);
        Ok(())
    }

    /// The code `name` names, if a running word may raise it, for a `try`
    /// to catch: the language's or the application's; else why not.
    pub(crate) fn caught(&self, name: &str) -> Result<Code, String> {
        let raised = |code: &&Code| code.raised() || self.codes.contains(code);
        let all = Code::ALL.iter().chain(&self.codes);
        if let Some(code) = all.clone().find(|c| c.name() == name) {
            return match raised(&code) {
                true => Ok(*code),
                false => Err(format!(
                    "{name} is found compiling, at the program's end or in the application, never raised by a running word: no try catches it"
                )),
            };
        }
        Err(format!("{name} is no code a word raises"))
    }

    /// How many values word `id` takes.
    pub(crate) fn takes(&self, id: usize) -> usize {
        self.words.get(id).map_or(0, |w| w.takes)
    }

    /// The words' names, by id.
    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.words.iter().map(|w| w.name.as_str())
    }

    /// What `value` left at a program's end is told, if it must be used.
    pub(crate) fn unused<A: AppValue>(&self, value: &Value<A>) -> Option<&str> {
        let Value::App(a) = value else {
            return None;
        };
        let found = self.unused.iter().find(|(name, _)| name == a.name());
        found.map(|(_, why)| why.as_str())
    }
}
