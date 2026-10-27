//! Compiling: a program's tokens, all of them, into flat lists of steps,
//! before anything runs. Names are found as they are compiled, so a word no
//! one defines, a block left open or a stray `end` is an error before the
//! program does anything.
//!
//! The parsing words are a fixed set, the only ones that read the token
//! after them or shape the program: `str`, `define NAME … end`,
//! `let NAME`, `if … else … then`, `begin … until`, `list … end`,
//! `each … end` and `try … catch … end`. Control flow is compiled to jumps,
//! its open blocks kept on a compile-time stack. Every other word takes and
//! gives values on the stack, and nothing else.
use crate::error::{Code, Error};
use crate::machine::Builtin;
use crate::token::Token;
use crate::value::Value;
pub use crate::vocabulary::Vocabulary;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

/// Expands `Variant "name", ...` to the enum `Parsing` of those variants,
/// `Parsing::ALL` in that order, and `name` and `reads`, matches giving
/// each one's: `If "if",` gives `Parsing::If`, named "if", reading no
/// token; `Str "str" Text,` reads the next as `Read::Text`.
macro_rules! parsing {
    (@reads) => { None };
    (@reads $read:ident) => { Some(Read::$read) };
    ($($variant:ident $name:literal $($read:ident)?,)*) => {
        /// The words that read the token after them, or shape the program:
        /// no definition or `let` may take their names.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Parsing {
            $($variant,)*
        }

        impl Parsing {
            pub const ALL: [Self; [$($name),*].len()] = [$(Self::$variant),*];

            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }

            /// What it reads of the token after it, if it reads one: that
            /// token is no word of the program's.
            pub fn reads(self) -> Option<Read> {
                match self {
                    $(Self::$variant => parsing!(@reads $($read)?),)*
                }
            }
        }
    };
}

parsing! {
    Str "str" Text,
    Define "define" Name,
    End "end",
    Let "let" Name,
    If "if",
    Else "else",
    Then "then",
    Begin "begin",
    Until "until",
    List "list",
    Each "each",
    Try "try",
    Catch "catch",
}

impl Builtin {
    /// The built-in `name` is, if one.
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.name() == name)
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
                "a word of your own; it may call itself (end also closes list, each and try)",
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
            Parsing::Try | Parsing::Catch => (
                "CODES try … catch … end",
                "the body from an empty stack, its values left; if it fails with one of \
                 CODES (str 'timeout closed' just before try), the handler instead, on \
                 the stack as it was at try and the failure's message (or the value it \
                 carries, as the application declares) and code",
            ),
        }
    }
}

/// What a parsing word reads of the token after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Read {
    /// The token as text, whatever it looks like (`str`).
    Text,
    /// The token as the name it gives a meaning (`define`, `let`).
    Name,
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
    /// A `let` name of the top level's, by its slot. Only the top level's
    /// code holds one (a `let` in a definition is its own): the checker
    /// keeps the top level's names and a definition's in one map.
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
    /// `try`: the stack from here is the body's; a failure of one of its
    /// codes (behind one pointer, so no step is larger for them) goes to
    /// its handler, the step given.
    Try(usize, Rc<Vec<Code>>),
    /// `catch`: the body went through; on past the handler, to the step
    /// given.
    Tried(usize),
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
    fn new() -> Self {
        Self {
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
        if let Some(
            Op::Jump(target)
            | Op::JumpUnless(target)
            | Op::EachNext(target)
            | Op::Tried(target)
            | Op::Try(target, _),
        ) = self.ops.get_mut(at)
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
    pub(crate) words: Vec<Rc<Steps<A>>>,
    pub(crate) globals: usize,
    pub(crate) vocabulary: Rc<Vocabulary>,
}

impl<A> Program<A> {
    /// A code by its index: `None` is the top level.
    pub(crate) fn code(&self, code: Option<usize>) -> Option<&Steps<A>> {
        match code {
            None => Some(&self.main),
            Some(i) => self.words.get(i).map(|w| &**w),
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

/// A block open as compiling goes: where its jumps are, to patch; and a
/// definition's `define`, its place in the source's tokens (`from`).
#[derive(Clone)]
enum Open {
    Define { at: Token, from: usize },
    If { jump: usize, at: Token },
    Else { jump: usize, at: Token },
    Begin { start: usize, at: Token },
    List { at: Token },
    Each { head: usize, at: Token },
    Try { step: usize, at: Token },
    Catch { jump: usize, at: Token },
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
/// program: the library is compiled and checked once, and its definitions
/// and what the checker knows of them are shared, not copied.
#[derive(Clone)]
pub struct Compiler<A> {
    vocabulary: Rc<Vocabulary>,
    names: HashMap<Rc<str>, Entry>,
    main: Steps<A>,
    words: Vec<Rc<Steps<A>>>,
    globals: usize,
    open: Vec<Open>,
    current: Option<Current<A>>,
    /// Whether the last step compiled is a number or `str` written just
    /// before, so a `pick` or `roll` after it may take it as its count, or
    /// a `try` as its codes.
    literal: bool,
    /// What the checker has worked out of the definitions so far.
    known: crate::check::Known,
    /// Calls to words not defined yet, each made by a definition (its
    /// index in `words`, or `None` while it is open) at a step: patched
    /// when the word is defined, an error if none is by the source's end.
    forward: Vec<(Token, Option<usize>, usize)>,
    /// The names the source being added defines: none may twice.
    defined: HashSet<Rc<str>>,
    /// Where each definition of the source being added is in its tokens:
    /// its `define`'s place and its `end`'s.
    made: Vec<(usize, usize)>,
    /// The names the source being added gave a meaning, with what each
    /// meant before.
    replaced: Vec<(Rc<str>, Option<Entry>)>,
    /// Definitions that read `let` names of the top level's, which a
    /// program compiled after these ([`definitions`](Compiler::definitions))
    /// may not call.
    closed: HashSet<usize>,
}

impl<A: Clone> Compiler<A> {
    pub fn new(vocabulary: Vocabulary) -> Self {
        let mut names = HashMap::new();
        for (id, name) in vocabulary.names().enumerate() {
            names.insert(Rc::from(name), Entry::App(id));
        }
        for builtin in Builtin::ALL {
            names.insert(Rc::from(builtin.name()), Entry::Builtin(builtin));
        }
        Self {
            vocabulary: Rc::new(vocabulary),
            names,
            main: Steps::new(),
            words: Vec::new(),
            globals: 0,
            open: Vec::new(),
            current: None,
            literal: false,
            known: crate::check::Known::default(),
            forward: Vec::new(),
            defined: HashSet::new(),
            made: Vec::new(),
            replaced: Vec::new(),
            closed: HashSet::new(),
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

    /// Compiles `tokens`, a source, onto what was compiled before; if it
    /// fails, the compiler is as it was. A definition in it may call one
    /// written after it.
    pub fn add(&mut self, tokens: &[Token]) -> Result<(), Error> {
        let (words, main, globals) = (self.words.len(), self.main.len(), self.globals);
        let (open, current) = (self.open.clone(), self.current.clone());
        self.defined.clear();
        self.made.clear();
        let added = self
            .source(tokens)
            .and_then(|()| match self.forward.first() {
                Some((token, ..)) => Err(self.unknown(token)),
                None => Ok(()),
            });
        let replaced = std::mem::take(&mut self.replaced);
        if added.is_err() {
            for (name, was) in replaced.into_iter().rev() {
                match was {
                    Some(entry) => self.names.insert(name, entry),
                    None => self.names.remove(&name),
                };
            }
            self.words.truncate(words);
            self.main.ops.truncate(main);
            self.main.tokens.truncate(main);
            (self.globals, self.open, self.current) = (globals, open, current);
            self.forward.clear();
        }
        added
    }

    /// `name` means `entry` from here on: what it meant before kept, for
    /// a source that fails to undo.
    fn name(&mut self, name: &Rc<str>, entry: Entry) {
        let was = self.names.insert(Rc::clone(name), entry);
        self.replaced.push((Rc::clone(name), was));
    }

    fn source(&mut self, all: &[Token]) -> Result<(), Error> {
        // A literal is part of the step after it in the same source alone.
        self.literal = false;
        let mut tokens = all.iter();
        // The place in `all` of the token `rest` gave last.
        let last = |rest: &std::slice::Iter<Token>| {
            (all.len().saturating_sub(rest.as_slice().len())).saturating_sub(1)
        };
        while let Some(token) = tokens.next() {
            let text = &*token.text;
            let literal = std::mem::take(&mut self.literal);
            match Parsing::named(text) {
                Some(Parsing::Str) => {
                    let next = tokens.next().ok_or_else(|| missing(token, "a string"))?;
                    self.code().push(Op::Push(Value::str(&*next.text)), token);
                    self.literal = true;
                }
                Some(Parsing::Define) => {
                    if !self.open.is_empty() {
                        let why = "define goes at the top level, outside any definition or block";
                        return Err(Error::new(Code::NotTopLevel, why, Some(token)));
                    }
                    let from = last(&tokens);
                    let name = tokens.next().ok_or_else(|| missing(token, "a name"))?;
                    self.check_name(name, false)?;
                    // Visible from here on, its own body included, and to
                    // the definitions before it that called it.
                    let at = self.words.len();
                    for (_, word, step) in self.forward.iter().filter(|f| f.0.text == name.text) {
                        let code = word.and_then(|w| self.words.get_mut(w)).map(Rc::make_mut);
                        if let Some(op) = code.and_then(|w| w.ops.get_mut(*step)) {
                            *op = Op::Call(at);
                        }
                    }
                    self.forward.retain(|f| f.0.text != name.text);
                    self.defined.insert(Rc::clone(&name.text));
                    self.name(&name.text, Entry::Word(at));
                    self.current = Some(Current {
                        code: Steps::new(),
                        locals: HashMap::new(),
                    });
                    self.open.push(Open::Define {
                        at: token.clone(),
                        from,
                    });
                }
                Some(Parsing::End) => match self.open.pop() {
                    Some(Open::Define { from, .. }) => {
                        self.made.push((from, last(&tokens)));
                        let at = self.words.len();
                        for (_, word, _) in self.forward.iter_mut().filter(|f| f.1.is_none()) {
                            *word = Some(at);
                        }
                        self.words
                            .extend(self.current.take().map(|c| Rc::new(c.code)))
                    }
                    Some(Open::List { .. }) => self.code().push(Op::Gather, token),
                    Some(Open::Each { head, .. }) => {
                        self.code().push(Op::Jump(head), token);
                        let out = self.code().len();
                        self.code().patch(head, out);
                    }
                    Some(Open::Catch { jump, .. }) => {
                        let out = self.code().len();
                        self.code().patch(jump, out);
                    }
                    other => {
                        return Err(mismatched(token, other, "no block that end closes is open"));
                    }
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
                                    self.name(&name.text, Entry::Global(slot));
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
                    other => return Err(mismatched(token, other, "no if is open")),
                },
                Some(Parsing::Then) => match self.open.pop() {
                    Some(Open::If { jump, .. } | Open::Else { jump, .. }) => {
                        let here = self.code().len();
                        self.code().patch(jump, here);
                    }
                    other => return Err(mismatched(token, other, "no if is open")),
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
                    other => return Err(mismatched(token, other, "no begin is open")),
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
                Some(Parsing::Try) => {
                    let codes = self.codes(token, literal)?;
                    let step = self.code().len();
                    self.code().push(Op::Try(0, codes), token);
                    self.open.push(Open::Try {
                        step,
                        at: token.clone(),
                    });
                }
                Some(Parsing::Catch) => match self.open.pop() {
                    // Said at its `try`, as an open block is.
                    Some(Open::Try { step, at }) => {
                        let jump = self.code().len();
                        self.code().push(Op::Tried(0), token);
                        self.code().patch(step, jump.saturating_add(1));
                        self.open.push(Open::Catch { jump, at });
                    }
                    other => return Err(mismatched(token, other, "no try is open")),
                },
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
            Some(Entry::Word(id)) if self.closed.contains(&id) => {
                return Err(Error::new(
                    Code::Unset,
                    "reads let names of the program it was defined in, which this one does not see",
                    Some(token),
                ));
            }
            Some(Entry::Word(id)) => {
                // A word in its own body isn't in `words` until its `end`:
                // a call.
                let body = self.words.get(id).filter(|w| w.leaf()).cloned();
                match body {
                    // Short and straight: copied in, each step named by the
                    // word the program wrote.
                    Some(body) => {
                        for op in body.ops.iter().cloned() {
                            self.code().push(op, token);
                        }
                    }
                    None => self.code().push(Op::Call(id), token),
                }
            }
            Some(Entry::Global(slot)) => self.code().push(Op::GetGlobal(slot), token),
            // Not known here: in a definition, perhaps one written later.
            None if self.current.is_some() && !text.is_empty() => {
                let step = self.code().len();
                self.code().push(Op::Call(usize::MAX), token);
                self.forward.push((token.clone(), None, step));
            }
            None => return Err(self.unknown(token)),
        }
        Ok(())
    }

    /// The codes a `try` catches: the string written just before it, the
    /// step that pushed it taken back. Each must be one a running word may
    /// raise: a misspelt one would catch nothing.
    fn codes(&mut self, token: &Token, literal: bool) -> Result<Rc<Vec<Code>>, Error> {
        let bad = |why: String| Error::new(Code::BadCodes, why, Some(token));
        let words = Rc::clone(&self.vocabulary);
        let Some(Op::Push(Value::Str(text))) = self.code().ops.last().filter(|_| literal) else {
            let why = "takes the codes it catches as a string written just before it: \
                       str timeout try, str 'timeout closed' try";
            return Err(bad(why.to_owned()));
        };
        let codes = (text.split_whitespace()).map(|name| words.caught(name));
        let codes = codes.collect::<Result<Vec<Code>, String>>().map_err(bad)?;
        if codes.is_empty() {
            return Err(bad("names no code to catch".to_owned()));
        }
        let code = self.code();
        code.ops.pop();
        code.tokens.pop();
        Ok(Rc::new(codes))
    }

    /// An unknown word, with its source and the explicit text syntax.
    fn unknown(&self, token: &Token) -> Error {
        Error::new(
            Code::UnknownWord,
            "no such word; text is `str WORD`",
            Some(token),
        )
    }

    /// Compiles `text`, a library named `source`, as `add` does, and gives
    /// each of its definitions' help, which the comment on the line just
    /// above its `define` says: `# EFFECT : ABOUT`. A definition without
    /// it is refused, and the compiler is as it was. A library is whole:
    /// it opens no block another source closes, nor closes one.
    pub fn library(&mut self, source: &str, text: &str) -> Result<Vec<Doc>, Error> {
        if let Some(open) = self.open.last() {
            return Err(unclosed(open));
        }
        let spans = crate::token::spans(source, text)?;
        let tokens: Vec<Token> = spans.iter().map(|(token, _)| token.clone()).collect();
        let before = self.clone();
        self.add(&tokens)?;
        let docs = match self.open.last() {
            Some(open) => Err(unclosed(open)),
            None => (self.made.iter())
                .map(|&(from, to)| Doc::of(text, &spans, from, to))
                .collect::<Result<Vec<Doc>, Error>>(),
        };
        if docs.is_err() {
            *self = before;
        }
        docs
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
            vocabulary: Rc::clone(&self.vocabulary),
        };
        crate::check::definitions(&program, &mut self.known);
    }

    /// A compiler for a program of its own that sees these definitions:
    /// not the top level, nor its `let` names, whose values are not that
    /// program's. A definition reading one, itself or through a call, may
    /// not be called there. Checked once, as `prepare` checks.
    pub fn definitions(&self) -> Self {
        let mut scope = self.clone();
        scope.main = Steps::new();
        scope
            .names
            .retain(|_, entry| !matches!(entry, Entry::Global(_)));
        let reads = |w: &Steps<A>, closed: &HashSet<usize>| {
            (w.ops.iter()).any(|op| {
                matches!(op, Op::GetGlobal(_)) || matches!(op, Op::Call(c) if closed.contains(c))
            })
        };
        // Through calls, to a word written later too: until none is added.
        while let Some(id) = (scope.words.iter().enumerate())
            .position(|(id, w)| !scope.closed.contains(&id) && reads(w, &scope.closed))
        {
            scope.closed.insert(id);
        }
        scope.prepare();
        scope
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

    /// Whether `name` may be defined (`define`) or named (`let`): either
    /// may take a library's or the host's word's name for itself, from
    /// there on; only the core's and the parsing words are kept, and a
    /// source defines a name once.
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
                Some(Entry::Word(_)) if !by_let && self.defined.contains(text) => {
                    Some("defined already")
                }
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

/// A library's definition, as help says it: its name, its stack effect
/// and what it does, from the comment just above it, and its code as
/// written, from `define` to `end`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    pub name: String,
    pub effect: String,
    pub about: String,
    pub code: String,
}

impl Doc {
    /// The help of the definition written in `text` from token `from` of
    /// `spans` (its `define`) to token `to` (its `end`).
    fn of(
        text: &str,
        spans: &[(Token, Range<usize>)],
        from: usize,
        to: usize,
    ) -> Result<Doc, Error> {
        let span = |i: usize| spans.get(i).map(|(_, span)| span.clone());
        let start = span(from).map_or(0, |s| s.start);
        let after = from.checked_sub(1).and_then(span).map_or(0, |s| s.end);
        // Between tokens is only whitespace and comments: a comment there
        // whole on the line above `define`'s, not after a token on it.
        let mut lines = text.get(after..start).unwrap_or_default().rsplit('\n');
        let above = lines.nth(1).filter(|_| from == 0 || lines.next().is_some());
        let said = above.and_then(|line| line.trim().strip_prefix('#'));
        let said = said.and_then(|said| said.trim().split_once(" : "));
        let name = spans
            .get(from.saturating_add(1))
            .map(|(token, _)| &*token.text);
        let define = spans.get(from).map(|(token, _)| token);
        let Some(((effect, about), name)) = said.zip(name) else {
            let why = "a library's definition says what it does on the line just above it: \
                       # EFFECT : ABOUT";
            return Err(Error::new(Code::Contract, why, define));
        };
        let code = span(to).and_then(|end| text.get(start..end.end));
        Ok(Doc {
            name: name.to_owned(),
            effect: effect.trim().to_owned(),
            about: about.trim().to_owned(),
            code: code.unwrap_or_default().to_owned(),
        })
    }
}

fn missing(token: &Token, what: &str) -> Error {
    Error::new(
        Code::MissingToken,
        format!("{} needs {what} after it", token.text),
        Some(token),
    )
}

fn opened(open: &Open) -> (&Token, &'static str) {
    match open {
        Open::Define { at, .. } => (at, "define … end"),
        Open::If { at, .. } => (at, "if … then"),
        Open::Else { at, .. } => (at, "else … then"),
        Open::Begin { at, .. } => (at, "begin … until"),
        Open::List { at } => (at, "list … end"),
        Open::Each { at, .. } => (at, "each … end"),
        Open::Try { at, .. } => (at, "try … catch"),
        Open::Catch { at, .. } => (at, "try … catch … end"),
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

/// `token`, which closes a block, closing `open`, not its own; with no
/// block open, `none` says what it lacks.
fn mismatched(token: &Token, open: Option<Open>, none: &str) -> Error {
    let Some(open) = open else {
        return Error::new(Code::Unmatched, none, Some(token));
    };
    let (at, what) = opened(&open);
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
