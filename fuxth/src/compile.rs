//! Compiling: a program's tokens, all of them, into flat lists of steps,
//! before anything runs. Names are found as they are compiled, so a word no
//! one defines, a block left open or a stray `end` is an error before the
//! program does anything.
//!
//! The parsing words are a fixed set, the only ones that read the token
//! after them or shape the program: `str`, `define NAME … end`,
//! `let NAME`, `if … else … then`, `begin … until`, `list … end` and
//! `each … end`. Control flow is compiled to jumps, its open blocks kept on
//! a compile-time stack. Every other word takes and gives values on the
//! stack, and nothing else.
use crate::error::{Code, Error};
use crate::machine::Builtin;
use crate::token::Token;
use crate::value::Value;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::rc::Rc;

/// Expands `Variant "name", ...` to the enum `Parsing` of those variants,
/// `PARSING`, their names, `Parsing::ALL` in that order, and `name`, a
/// match giving each one's: `If "if",` gives `Parsing::If`, named "if".
macro_rules! parsing {
    ($($variant:ident $name:literal,)*) => {
        /// The words that read the token after them, or shape the program:
        /// no definition or `let` may take their names.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Parsing {
            $($variant,)*
        }

        /// The parsing words' names, in `Parsing::ALL`'s order.
        pub const PARSING: [&str; [$($name),*].len()] = [$($name),*];

        impl Parsing {
            pub const ALL: [Parsing; PARSING.len()] = [$(Parsing::$variant),*];

            pub fn name(self) -> &'static str {
                match self {
                    $(Parsing::$variant => $name,)*
                }
            }
        }
    };
}

parsing! {
    Str "str",
    Define "define",
    End "end",
    Let "let",
    If "if",
    Else "else",
    Then "then",
    Begin "begin",
    Until "until",
    List "list",
    Each "each",
}

impl Builtin {
    /// The built-in `name` is, if one.
    pub fn named(name: &str) -> Option<Builtin> {
        Builtin::ALL.into_iter().find(|b| b.name() == name)
    }
}

impl Parsing {
    /// The parsing word `name` is, if one.
    pub fn named(name: &str) -> Option<Parsing> {
        Parsing::ALL.into_iter().find(|p| p.name() == name)
    }

    /// How it is written, and what it does, as help says it: the words of
    /// a block share the block's.
    pub fn syntax(self) -> (&'static str, &'static str) {
        match self {
            Parsing::If | Parsing::Else | Parsing::Then => {
                ("BOOL if … else … then", "runs one branch")
            }
            Parsing::Begin | Parsing::Until => {
                ("begin … BOOL until", "runs until the bool is true")
            }
            Parsing::Define | Parsing::End => (
                "define NAME … end",
                "a word of your own; it may call itself (end also closes list and each)",
            ),
            Parsing::Let => (
                "X let NAME",
                "NAME pushes X from then on (in a definition, for that call)",
            ),
            Parsing::List => (
                "list … end",
                "a list of the values its body leaves; the body starts with an empty stack",
            ),
            Parsing::Each => (
                "SEQ each … end",
                "the body once per element of SEQ, a string or list, the element pushed",
            ),
            Parsing::Str => ("str WORD", "WORD as text, whatever it looks like"),
        }
    }
}

/// The application's words: each a name, how many values it takes, and
/// how many it gives (a range, for a word that gives a value only
/// sometimes).
#[derive(Clone, Debug, Default)]
pub struct Vocabulary {
    words: Vec<(String, usize, RangeInclusive<usize>)>,
}

impl Vocabulary {
    /// Adds a word taking `takes` values and giving `gives`; its id, which
    /// the host is called with. A name given twice keeps its first word.
    pub fn add(&mut self, name: &str, takes: usize, gives: RangeInclusive<usize>) -> usize {
        if let Some(id) = self.words.iter().position(|(n, _, _)| n == name) {
            return id;
        }
        self.words.push((name.to_owned(), takes, gives));
        self.words.len().saturating_sub(1)
    }

    /// How many values the word `id` takes, and how many it gives: none,
    /// for an id no word has.
    pub(crate) fn effect(&self, id: usize) -> (usize, RangeInclusive<usize>) {
        self.words
            .get(id)
            .map_or((0, 0..=0), |(_, takes, gives)| (*takes, gives.clone()))
    }
}

/// One step of compiled code.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Op<A> {
    Push(Value<A>),
    Builtin(Builtin),
    /// `pick` with its count written just before it.
    PickN(usize),
    /// `roll` with its count written just before it.
    RollN(usize),
    App(usize),
    /// A definition, by its index.
    Call(usize),
    Jump(usize),
    /// Takes a boolean; jumps if it is false.
    JumpUnless(usize),
    /// A `let` name of the definition's own, by its slot.
    SetLocal(usize),
    GetLocal(usize),
    /// A `let` name of the top level's, by its slot.
    SetGlobal(usize),
    GetGlobal(usize),
    /// `list`: the stack from here is the list's.
    Mark,
    /// `end` of a `list`: what is above the mark, gathered into a list.
    Gather,
    /// `each`: takes the string or list to walk.
    EachStart,
    /// The head of an `each` loop: the next element pushed, or, past the
    /// last, a jump out to the step given.
    EachNext(usize),
}

/// Steps, each with the token it came from, and how many `let` names of
/// its own a definition keeps.
#[derive(Clone, Debug)]
pub(crate) struct Steps<A> {
    pub ops: Vec<Op<A>>,
    pub tokens: Vec<Token>,
    pub locals: usize,
}

impl<A> Steps<A> {
    fn new() -> Steps<A> {
        Steps {
            ops: Vec::new(),
            tokens: Vec::new(),
            locals: 0,
        }
    }
    fn push(&mut self, op: Op<A>, token: &Token) {
        self.ops.push(op);
        self.tokens.push(token.clone());
    }
    fn len(&self) -> usize {
        self.ops.len()
    }
    /// Points the jump at `at` to `to`.
    fn patch(&mut self, at: usize, to: usize) {
        if let Some(Op::Jump(target) | Op::JumpUnless(target) | Op::EachNext(target)) =
            self.ops.get_mut(at)
        {
            *target = to;
        }
    }

    /// Whether it is one step, a literal or a built-in, to copy in at each
    /// call (`dup` is `0 pick`). Longer words stay calls, so a mistake in
    /// one is said as the whole word's, not one of its steps'.
    fn leaf(&self) -> bool {
        let [op] = self.ops.as_slice() else {
            return false;
        };
        matches!(
            op,
            Op::Push(_) | Op::Builtin(_) | Op::PickN(_) | Op::RollN(_)
        )
    }
}

/// A compiled program: its top level, and its definitions.
#[derive(Clone, Debug)]
pub struct Program<A> {
    pub(crate) main: Steps<A>,
    pub(crate) words: Vec<Steps<A>>,
    pub(crate) globals: usize,
    pub(crate) vocabulary: Vocabulary,
}

impl<A> Program<A> {
    /// A code by its index: `None` is the top level.
    pub(crate) fn code(&self, code: Option<usize>) -> Option<&Steps<A>> {
        match code {
            None => Some(&self.main),
            Some(i) => self.words.get(i),
        }
    }

    /// The token step `step` of code `code` came from.
    pub(crate) fn token(&self, code: Option<usize>, step: usize) -> Option<&Token> {
        self.code(code)?.tokens.get(step)
    }
}

/// What a name compiles to.
#[derive(Clone, Copy, Debug)]
enum Entry {
    Builtin(Builtin),
    App(usize),
    Word(usize),
    Global(usize),
}

/// A block open as compiling goes: where its jumps are, to patch.
#[derive(Clone)]
enum Open {
    Define { at: Token },
    If { jump: usize, at: Token },
    Else { jump: usize, at: Token },
    Begin { start: usize, at: Token },
    List { at: Token },
    Each { head: usize, at: Token },
}

/// The definition being compiled: its steps, and its own `let` names. It
/// goes into `words` at its `end`; until then nothing else can, so its index
/// is the next one there.
#[derive(Clone)]
struct Current<A> {
    code: Steps<A>,
    locals: HashMap<Rc<str>, usize>,
}

/// Compiles programs: sources added in order, all into one, then finished.
/// A library's definitions first, then a script, say. A compiler with a
/// library in, [`prepare`](Compiler::prepare)d, can be cloned for each
/// program: the library is compiled and checked once.
#[derive(Clone)]
pub struct Compiler<A> {
    vocabulary: Vocabulary,
    names: HashMap<Rc<str>, Entry>,
    main: Steps<A>,
    words: Vec<Steps<A>>,
    globals: usize,
    open: Vec<Open>,
    current: Option<Current<A>>,
    /// Whether the last step compiled is a number written just before, so
    /// a `pick` or `roll` after it may take it as its count.
    literal: bool,
    /// What the checker has worked out of the definitions so far.
    known: crate::check::Known,
}

impl<A: Clone> Compiler<A> {
    pub fn new(vocabulary: Vocabulary) -> Compiler<A> {
        let mut names = HashMap::new();
        for (id, (name, _, _)) in vocabulary.words.iter().enumerate() {
            names.insert(Rc::from(name.as_str()), Entry::App(id));
        }
        // The core's words are the core's: an application's word of the
        // same name is never reached.
        for builtin in Builtin::ALL {
            names.insert(Rc::from(builtin.name()), Entry::Builtin(builtin));
        }
        Compiler {
            vocabulary,
            names,
            main: Steps::new(),
            words: Vec::new(),
            globals: 0,
            open: Vec::new(),
            current: None,
            literal: false,
            known: crate::check::Known::default(),
        }
    }

    /// The code being compiled into: the definition open, else the top
    /// level.
    fn code(&mut self) -> &mut Steps<A> {
        match &mut self.current {
            Some(current) => &mut current.code,
            None => &mut self.main,
        }
    }

    /// Compiles `tokens` onto what was compiled before.
    pub fn add(&mut self, tokens: &[Token]) -> Result<(), Error> {
        let mut tokens = tokens.iter();
        while let Some(token) = tokens.next() {
            let text = &*token.text;
            let literal = std::mem::replace(&mut self.literal, false);
            match Parsing::named(text) {
                Some(Parsing::Str) => {
                    let next = tokens.next().ok_or_else(|| missing(token, "a string"))?;
                    self.code().push(Op::Push(Value::str(&*next.text)), token);
                }
                Some(Parsing::Define) => {
                    self.top_level(token)?;
                    let name = tokens.next().ok_or_else(|| missing(token, "a name"))?;
                    self.check_name(name, false)?;
                    // Visible from here on, its own body included.
                    let id = Entry::Word(self.words.len());
                    self.names.insert(Rc::clone(&name.text), id);
                    self.current = Some(Current {
                        code: Steps::new(),
                        locals: HashMap::new(),
                    });
                    self.open.push(Open::Define { at: token.clone() });
                }
                Some(Parsing::End) => match self.open.pop() {
                    Some(Open::Define { .. }) => {
                        self.words.extend(self.current.take().map(|c| c.code))
                    }
                    Some(Open::List { .. }) => self.code().push(Op::Gather, token),
                    Some(Open::Each { head, .. }) => {
                        self.code().push(Op::Jump(head), token);
                        let out = self.code().len();
                        self.code().patch(head, out);
                    }
                    Some(other) => return Err(mismatched(token, &other)),
                    None => return Err(unmatched(token, "no define, list or each is open")),
                },
                Some(Parsing::Let) => {
                    let name = tokens.next().ok_or_else(|| missing(token, "a name"))?;
                    self.check_name(name, true)?;
                    let op = match &mut self.current {
                        Some(current) => {
                            let next = current.locals.len();
                            let slot = *current.locals.entry(Rc::clone(&name.text)).or_insert(next);
                            current.code.locals = current.locals.len();
                            Op::SetLocal(slot)
                        }
                        None => {
                            let slot = match self.names.get(&name.text) {
                                Some(Entry::Global(slot)) => *slot,
                                Some(Entry::Builtin(_) | Entry::App(_) | Entry::Word(_)) | None => {
                                    let slot = self.globals;
                                    self.globals = slot.saturating_add(1);
                                    self.names
                                        .insert(Rc::clone(&name.text), Entry::Global(slot));
                                    slot
                                }
                            };
                            Op::SetGlobal(slot)
                        }
                    };
                    self.code().push(op, token);
                }
                Some(Parsing::If) => {
                    let jump = self.code().len();
                    self.code().push(Op::JumpUnless(0), token);
                    self.open.push(Open::If {
                        jump,
                        at: token.clone(),
                    });
                }
                Some(Parsing::Else) => match self.open.pop() {
                    Some(Open::If { jump, .. }) => {
                        let here = self.code().len();
                        self.code().push(Op::Jump(0), token);
                        self.code().patch(jump, here.saturating_add(1));
                        self.open.push(Open::Else {
                            jump: here,
                            at: token.clone(),
                        });
                    }
                    Some(other) => return Err(mismatched(token, &other)),
                    None => return Err(unmatched(token, "no if is open")),
                },
                Some(Parsing::Then) => match self.open.pop() {
                    Some(Open::If { jump, .. } | Open::Else { jump, .. }) => {
                        let here = self.code().len();
                        self.code().patch(jump, here);
                    }
                    Some(other) => return Err(mismatched(token, &other)),
                    None => return Err(unmatched(token, "no if is open")),
                },
                Some(Parsing::Begin) => {
                    let start = self.code().len();
                    self.open.push(Open::Begin {
                        start,
                        at: token.clone(),
                    });
                }
                Some(Parsing::Until) => match self.open.pop() {
                    Some(Open::Begin { start, .. }) => {
                        self.code().push(Op::JumpUnless(start), token);
                    }
                    Some(other) => return Err(mismatched(token, &other)),
                    None => return Err(unmatched(token, "no begin is open")),
                },
                Some(Parsing::List) => {
                    self.code().push(Op::Mark, token);
                    self.open.push(Open::List { at: token.clone() });
                }
                Some(Parsing::Each) => {
                    self.code().push(Op::EachStart, token);
                    let head = self.code().len();
                    self.code().push(Op::EachNext(0), token);
                    self.open.push(Open::Each {
                        head,
                        at: token.clone(),
                    });
                }
                None => self.word(token, text, literal)?,
            }
        }
        Ok(())
    }

    /// A word that is not a parsing word: a number, or a name.
    fn word(&mut self, token: &Token, text: &str, literal: bool) -> Result<(), Error> {
        if is_number(text) {
            let Ok(n) = text.parse::<i64>() else {
                return Err(Error::new(
                    Code::Overflow,
                    "a number past what an integer holds (64 bits)",
                    Some(token),
                ));
            };
            self.code().push(Op::Push(Value::Int(n)), token);
            self.literal = true;
            return Ok(());
        }
        let local = (self.current.as_ref()).and_then(|c| c.locals.get(text).copied());
        if let Some(slot) = local {
            self.code().push(Op::GetLocal(slot), token);
            return Ok(());
        }
        let entry = self.names.get(text).copied();
        match entry {
            Some(Entry::Builtin(b @ (Builtin::Pick | Builtin::Roll))) => {
                // `3 pick`: the count is known, so the step is one.
                let count = match (literal, self.code().ops.last()) {
                    (true, Some(Op::Push(Value::Int(n)))) => usize::try_from(*n).ok(),
                    _ => None,
                };
                match count {
                    Some(n) => {
                        let code = self.code();
                        code.ops.pop();
                        code.tokens.pop();
                        let op = if b == Builtin::Pick {
                            Op::PickN(n)
                        } else {
                            Op::RollN(n)
                        };
                        code.push(op, token);
                    }
                    None => self.code().push(Op::Builtin(b), token),
                }
            }
            Some(Entry::Builtin(b)) => self.code().push(Op::Builtin(b), token),
            Some(Entry::App(id)) => self.code().push(Op::App(id), token),
            Some(Entry::Word(id)) => {
                // A word in its own body isn't in `words` until its `end`:
                // a call.
                let body = self.words.get(id).filter(|w| w.leaf()).cloned();
                match body {
                    // Short and straight: copied in, each step named by the
                    // word the program wrote.
                    Some(body) => {
                        for op in body.ops {
                            self.code().push(op, token);
                        }
                    }
                    None => self.code().push(Op::Call(id), token),
                }
            }
            Some(Entry::Global(slot)) => self.code().push(Op::GetGlobal(slot), token),
            None => {
                return Err(Error::new(
                    Code::UnknownWord,
                    if text.is_empty() {
                        "no such word: an empty one (str '' pushes an empty string)"
                    } else {
                        "no such word"
                    },
                    Some(token),
                ));
            }
        }
        Ok(())
    }

    /// Checks the definitions compiled so far, once, so that clones of
    /// this compiler don't check them again: call it after a library.
    pub fn prepare(&mut self) {
        if self.current.is_some() || !self.open.is_empty() {
            return;
        }
        let program = Program {
            main: Steps::new(),
            words: self.words.clone(),
            globals: self.globals,
            vocabulary: self.vocabulary.clone(),
        };
        crate::check::definitions(&program, &mut self.known);
    }

    /// The program, every block closed.
    pub fn finish(self) -> Result<Program<A>, Error> {
        if let Some(open) = self.open.last() {
            return Err(unclosed(open));
        }
        let program = Program {
            main: self.main,
            words: self.words,
            globals: self.globals,
            vocabulary: self.vocabulary,
        };
        crate::check::check(&program, self.known)?;
        Ok(program)
    }

    fn top_level(&self, token: &Token) -> Result<(), Error> {
        if self.open.is_empty() {
            return Ok(());
        }
        Err(Error::new(
            Code::NotTopLevel,
            "define goes at the top level, outside any definition or block",
            Some(token),
        ))
    }

    /// Whether `name` may be defined (`define`), or named (`let`, which may
    /// take a library or host word's name for itself: only the core and the
    /// parsing words are kept).
    fn check_name(&self, name: &Token, by_let: bool) -> Result<(), Error> {
        let text = &*name.text;
        let why = if text.is_empty() {
            Some("an empty name")
        } else if is_number(text) {
            Some("a number")
        } else if Parsing::named(text).is_some() {
            Some("a word of the language")
        } else {
            match self.names.get(text) {
                Some(Entry::Builtin(_)) => Some("a word of the core"),
                Some(Entry::App(_) | Entry::Word(_)) if !by_let => Some("a word there already"),
                Some(Entry::Global(_)) if !by_let => Some("a let name there already"),
                Some(Entry::App(_) | Entry::Word(_) | Entry::Global(_)) | None => None,
            }
        };
        match why {
            None => Ok(()),
            Some(why) => Err(Error::new(
                Code::BadName,
                format!("{text:?} cannot be defined: it is {why}"),
                Some(name),
            )),
        }
    }
}

fn missing(token: &Token, what: &str) -> Error {
    Error::new(
        Code::MissingToken,
        format!("{} needs {what} after it", token.text),
        Some(token),
    )
}

fn unmatched(token: &Token, why: &str) -> Error {
    Error::new(Code::Unmatched, why, Some(token))
}

fn opened(open: &Open) -> (&Token, &'static str) {
    match open {
        Open::Define { at } => (at, "define … end"),
        Open::If { at, .. } => (at, "if … then"),
        Open::Else { at, .. } => (at, "else … then"),
        Open::Begin { at, .. } => (at, "begin … until"),
        Open::List { at } => (at, "list … end"),
        Open::Each { at, .. } => (at, "each … end"),
    }
}

fn unclosed(open: &Open) -> Error {
    let (at, what) = opened(open);
    Error::new(
        Code::Unclosed,
        format!("this {what} is never closed"),
        Some(at),
    )
}

fn mismatched(token: &Token, open: &Open) -> Error {
    let (at, what) = opened(open);
    Error::new(
        Code::Unmatched,
        format!("the open block is {what}, from {}", at.place),
        Some(token),
    )
}

/// Whether `text` is written as an integer: digits, after a `-` or not.
/// Nothing else is a number: `+5` and `1e3` are names.
fn is_number(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}
