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
/// its values' `AppValue::name` says it), as an error says it, what one
/// left over is told if a value of it must be used, and the kinds taken
/// where an effect names it; and the names
/// effects use for several kinds; and the codes of its failures. The
/// language's kinds are `integer`, `boolean`, `string` and `list`, and
/// `any` is any value. Each name means one thing: a second kind or name
/// of effects called as one is, a second word called as another word is
/// (the core's and the parsing words among them), or a code declared
/// twice is refused, never kept beside the first or in its place.
#[derive(Clone, Debug, Default)]
pub struct Vocabulary {
    words: Vec<Word>,
    kinds: Vec<(String, String, Option<String>, Kinds)>,
    names: Vec<(String, Map, String)>,
    codes: Vec<Code>,
    /// The codes whose failures may carry a value, which a `try`'s handler
    /// gets in place of the message, and its kinds.
    carried: Vec<(Code, Kinds)>,
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
        self.unnamed(name)?;
        if Kinds::host(self.kinds.len()) == Kinds::NONE {
            return Err(refused(format!("{name}: a kind too many")));
        }
        let i = self.kinds.len();
        self.kinds
            .push((name.to_owned(), said.to_owned(), None, Kinds::host(i)));
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

    /// Where a word's effect names kind `name`, a value of kind `also` is
    /// taken too, and may be given (an action's answer is its pane): said
    /// before any word, so none read one way and another the other. A name
    /// of effects (`name`) reads each kind as itself, so it widens none,
    /// and neither does another kind `also` is taken as.
    pub fn taken_as(&mut self, name: &str, also: &str) -> Result<(), Error> {
        if !self.words.is_empty() {
            return Err(refused(format!(
                "{name}: taken as more after a word was added"
            )));
        }
        let also = taken(&self.lookup(also, false)?.0);
        let kind = self.kinds.iter_mut().find(|(n, ..)| n == name);
        let kind = kind.ok_or_else(|| refused(format!("{name}: no such kind")))?;
        if kind.3.union(also) == kind.3 {
            return Err(refused(format!("{name}: taken as that already")));
        }
        kind.3 = kind.3.union(also);
        Ok(())
    }

    /// A name effects may use for several kinds, `kinds` their names (a
    /// kind's, or another such name's), `said` saying them in an error;
    /// `from>to` is a kind that becomes another when passed through.
    pub fn name(&mut self, name: &str, kinds: &str, said: &str) -> Result<(), Error> {
        self.unnamed(name)?;
        let mut map = Map::new();
        for part in kinds.split_whitespace() {
            let (from, to) = part.split_once('>').unwrap_or((part, part));
            let (from, to) = (self.lookup(from, false)?.0, self.lookup(to, false)?.0);
            map.push((taken(&from), taken(&to)));
        }
        self.names.push((name.to_owned(), map, said.to_owned()));
        Ok(())
    }

    /// Refuses `name` for a kind or a name of effects if an effect's
    /// `name` means something already.
    fn unnamed(&self, name: &str) -> Result<(), Error> {
        match self.lookup(name, false) {
            Ok(_) => Err(refused(format!("{name}: a kind or name already"))),
            Err(_) => Ok(()),
        }
    }

    /// What a name in an effect stands for, and how an error says it: a
    /// host kind with what it is taken as too if `widened`.
    fn lookup(&self, name: &str, widened: bool) -> Result<(Map, String), Error> {
        if let Some((_, map, said)) = self.names.iter().find(|(n, ..)| n == name) {
            return Ok((map.clone(), said.clone()));
        }
        let host = (self.kinds.iter().enumerate()).find(|(_, (n, ..))| n == name);
        let host = host.map(|(i, kind)| if widened { kind.3 } else { Kinds::host(i) });
        let core = Kind::ALL
            .into_iter()
            .find(|k| k.name() == name)
            .map(Kinds::one);
        let any = (name == "any").then_some(ANY);
        let Some(kinds) = host.or(core).or(any) else {
            return Err(refused(format!(
                "{name}: no kind or name of the vocabulary's"
            )));
        };
        Ok((vec![(kinds, kinds)], self.describe(kinds)))
    }

    /// Adds a word, `effect` saying what it takes and gives in the names
    /// of kinds, `--` between (`pane text -- answer`); its id, which the
    /// host is called with, the number of words added before it. A word
    /// is added once, under a name none of the core's or parsing words
    /// has: one name, one word.
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
        let Some((before, after)) = effect.split_once("--") else {
            return Err(refused(format!("{name}: no -- in {effect:?}")));
        };
        let lookup =
            |p| (self.lookup(p, true)).map_err(|e| refused(format!("{name}: {}", e.message)));
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

    /// The application's failures of `code` may carry a value of `name`'s
    /// kinds (`Machine::raise_with`): a `try`'s handler gets it in place of
    /// the message.
    pub fn carries(&mut self, code: Code, name: &str) -> Result<(), Error> {
        let kinds = taken(&self.lookup(name, false)?.0);
        self.carried.push((code, kinds));
        Ok(())
    }

    /// What the handler of a `try` that names `codes` gets in place of the
    /// message: a string, or what a failure of one of them carries.
    pub(crate) fn caught_kinds(&self, codes: &[Code]) -> Kinds {
        let carried = self.carried.iter().filter(|(code, _)| codes.contains(code));
        carried.fold(crate::check::STR, |all, (_, kinds)| all.union(*kinds))
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
        for (i, (_, _, unused, _)) in self.kinds.iter().enumerate() {
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
        let host = host.map(|(_, (_, said, ..))| said.as_str());
        let said: Vec<&str> = core
            .map(|kind| -> &str { kind.said() })
            .chain(host)
            .collect();
        listed(&said)
    }
}
