//! The application's words and the kinds of its values. Each word says
//! what it takes and gives as a stack effect in the names of kinds (`pane
//! text -- answer`), read strictly: the checker follows those kinds, and
//! the machine holds what the application gives to them.
use crate::check::{ANY, Kinds};
use crate::error::{Code, Error};
use crate::value::{AppValue, Kind, Value};

/// A name's kinds, each with what a value of it is once the name passes
/// it through (`x secs -- x`): itself, for most.
type Map = Vec<(Kinds, Kinds)>;

/// The kinds `map` makes of a value of `kinds`.
pub(crate) fn through(map: &Map, kinds: Kinds) -> Kinds {
    let made = map.iter().filter(|(from, _)| from.may_be(kinds));
    made.fold(Kinds::NONE, |all, (_, to)| all.union(*to))
}

/// The kinds a name takes.
fn taken(map: &Map) -> Kinds {
    map.iter()
        .fold(Kinds::NONE, |all, (from, _)| all.union(*from))
}

/// What a word takes in each place, the deepest first, with how an error
/// says it; and what it gives in each: the kinds its name maps, of those
/// taken in the place given if the name is among what it takes too.
#[derive(Clone, Debug)]
pub(crate) struct Word {
    name: String,
    pub(crate) takes: Vec<(Kinds, String)>,
    pub(crate) gives: Vec<(Option<usize>, Map)>,
}

/// The application's words; the kinds of its values, each its name (as
/// its values' `AppValue::name` says it), as an error says it, and what
/// one left over is told if a value of it must be used; and the names
/// effects use for several kinds; and the codes of its failures. The
/// language's kinds are `integer`, `boolean`, `string` and `list`, and
/// `any` is any value.
#[derive(Clone, Debug, Default)]
pub struct Vocabulary {
    words: Vec<Word>,
    kinds: Vec<(String, String, Option<String>)>,
    names: Vec<(String, Map, String)>,
    codes: Vec<&'static str>,
}

/// `said`, the last two joined by "or": "a, b or c".
pub(crate) fn listed(said: &[&str]) -> String {
    match said {
        [rest @ .., last] if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
        [one] => (*one).to_owned(),
        _ => String::new(),
    }
}

fn refused(message: String) -> Error {
    Error::new(Code::Contract, message, None)
}

impl Vocabulary {
    /// A kind of the application's values, `said` as an error says it ("a
    /// pane"); 27 at most.
    pub fn kind(&mut self, name: &str, said: &str) -> Result<(), Error> {
        if Kinds::host(self.kinds.len()) == Kinds::NONE {
            return Err(refused(format!("{name}: a kind too many")));
        }
        self.kinds.push((name.to_owned(), said.to_owned(), None));
        Ok(())
    }

    /// Values of kind `name` must be used: one left on the stack at a
    /// program's end is an error, saying `why`.
    pub fn must_use(&mut self, name: &str, why: &str) -> Result<(), Error> {
        let kind = self.kinds.iter_mut().find(|(n, ..)| n == name);
        kind.ok_or_else(|| refused(format!("{name}: no such kind")))?
            .2 = Some(why.to_owned());
        Ok(())
    }

    /// A name effects may use for several kinds, `kinds` their names (a
    /// kind's, or another such name's), `said` saying them in an error;
    /// `from>to` is a kind that becomes another when passed through.
    pub fn name(&mut self, name: &str, kinds: &str, said: &str) -> Result<(), Error> {
        let mut map = Map::new();
        for part in kinds.split_whitespace() {
            let (from, to) = part.split_once('>').unwrap_or((part, part));
            map.push((taken(&self.lookup(from)?.0), taken(&self.lookup(to)?.0)));
        }
        self.names.push((name.to_owned(), map, said.to_owned()));
        Ok(())
    }

    /// What a name in an effect stands for, and how an error says it.
    fn lookup(&self, name: &str) -> Result<(Map, String), Error> {
        if let Some((_, map, said)) = self.names.iter().rev().find(|(n, ..)| n == name) {
            return Ok((map.clone(), said.clone()));
        }
        let host = self.kinds.iter().position(|(n, ..)| n == name);
        let core = Kind::ALL
            .into_iter()
            .find(|k| k.name() == name)
            .map(Kinds::one);
        let any = (name == "any").then_some(ANY);
        let Some(kinds) = host.map(Kinds::host).or(core).or(any) else {
            return Err(refused(format!(
                "{name}: no kind or name of the vocabulary's"
            )));
        };
        Ok((vec![(kinds, kinds)], self.describe(kinds)))
    }

    /// Adds a word, `effect` saying what it takes and gives in the names
    /// of kinds, `--` between (`pane text -- answer`); its id, which the
    /// host is called with. A name given twice keeps its first word.
    pub fn add(&mut self, name: &str, effect: &str) -> Result<usize, Error> {
        if let Some(id) = self.words.iter().position(|w| w.name == name) {
            return Ok(id);
        }
        let Some((before, after)) = effect.split_once("--") else {
            return Err(refused(format!("{name}: no -- in {effect:?}")));
        };
        let lookup = |p| (self.lookup(p)).map_err(|e| refused(format!("{name}: {}", e.message)));
        let before: Vec<&str> = before.split_whitespace().collect();
        let takes = before
            .iter()
            .map(|p| lookup(p).map(|(map, said)| (taken(&map), said)));
        let gives = (after.split_whitespace())
            .map(|p| Ok((before.iter().position(|q| *q == p), lookup(p)?.0)));
        let word = Word {
            name: name.to_owned(),
            takes: takes.collect::<Result<_, Error>>()?,
            gives: gives.collect::<Result<_, Error>>()?,
        };
        self.words.push(word);
        Ok(self.words.len().saturating_sub(1))
    }

    /// A code the application's words may fail with (`Code::Host`), which
    /// a `try` may name: a word, and none of the language's own.
    pub fn code(&mut self, name: &'static str) -> Result<(), Error> {
        if name.is_empty() || name.contains(char::is_whitespace) {
            return Err(refused(format!("{name:?}: a code is one word")));
        }
        if Code::ALL.iter().any(|c| c.name() == name) {
            return Err(refused(format!("{name}: a code of the language's")));
        }
        self.codes.push(name);
        Ok(())
    }

    /// The code `name` names, if a running word may raise it, for a `try`
    /// to catch: the language's or the application's; else why not.
    pub(crate) fn caught(&self, name: &str) -> Result<Code, String> {
        if let Some(code) = Code::ALL.iter().find(|c| c.name() == name) {
            return match code.raised() {
                true => Ok(*code),
                false => Err(format!(
                    "{name} is found compiling, at the program's end or in the application, never raised by a running word: no try catches it"
                )),
            };
        }
        if let Some(code) = self.codes.iter().find(|c| **c == name) {
            return Ok(Code::Host(code));
        }
        let raised = (Code::ALL.iter().filter(|c| c.raised())).map(|c| c.name());
        let meant = crate::compile::meant(name, raised.chain(self.codes.iter().copied()));
        Err(format!("{name} is no code a word raises{meant}"))
    }

    pub(crate) fn word(&self, id: usize) -> Option<&Word> {
        self.words.get(id)
    }

    /// The words' names, by id.
    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.words.iter().map(|w| w.name.as_str())
    }

    /// The kind a value is.
    pub(crate) fn kind_of<A: AppValue>(&self, value: &Value<A>) -> Kinds {
        let Value::App(a) = value else {
            return Kinds::one(Kind::of(value));
        };
        let kind = self.kinds.iter().position(|(n, ..)| n == a.name());
        kind.map_or(Kinds::one(Kind::App), Kinds::host)
    }

    /// What one left of `kinds` at a program's end is told, if a value of
    /// them surely must be used.
    pub(crate) fn unused(&self, kinds: Kinds) -> Option<&str> {
        let mut why = None;
        let mut rest = kinds;
        for (i, (.., unused)) in self.kinds.iter().enumerate() {
            if let Some(unused) = unused.as_deref().filter(|_| kinds.may_be(Kinds::host(i))) {
                why = why.or(Some(unused));
                rest = rest.and(Kinds::host(i).not());
            }
        }
        why.filter(|_| rest == Kinds::NONE)
    }

    /// Kinds as an error says them: "an integer or a pane".
    pub(crate) fn describe(&self, kinds: Kinds) -> String {
        if kinds == ANY {
            return "any value".to_owned();
        }
        let core = Kind::ALL
            .into_iter()
            .filter(|kind| kinds.may_be(Kinds::one(*kind)));
        let host = (self.kinds.iter().enumerate()).filter(|(i, _)| kinds.may_be(Kinds::host(*i)));
        let host = host.map(|(_, (_, said, _))| said.as_str());
        let said: Vec<&str> = core
            .map(|kind| -> &str { kind.said() })
            .chain(host)
            .collect();
        listed(&said)
    }
}
