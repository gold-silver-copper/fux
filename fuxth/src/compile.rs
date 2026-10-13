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
use crate::token::Token;
use crate::value::Value;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::rc::Rc;

/// The words fuxth itself gives: its core. Everything else is the
/// standard library, written in fuxth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Builtin {
    Drop,
    Pick,
    Roll,
    Add,
    Sub,
    Mul,
    Div,
    Lt,
    Eq,
    Len,
    At,
    Slice,
    Concat,
    Push,
    Put,
    Code,
    Char,
    Kind,
    String,
    Print,
    Fail,
}

impl Builtin {
    pub const ALL: [Builtin; 21] = [
        Builtin::Drop,
        Builtin::Pick,
        Builtin::Roll,
        Builtin::Add,
        Builtin::Sub,
        Builtin::Mul,
        Builtin::Div,
        Builtin::Lt,
        Builtin::Eq,
        Builtin::Len,
        Builtin::At,
        Builtin::Slice,
        Builtin::Concat,
        Builtin::Push,
        Builtin::Put,
        Builtin::Code,
        Builtin::Char,
        Builtin::Kind,
        Builtin::String,
        Builtin::Print,
        Builtin::Fail,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Builtin::Drop => "drop",
            Builtin::Pick => "pick",
            Builtin::Roll => "roll",
            Builtin::Add => "add",
            Builtin::Sub => "sub",
            Builtin::Mul => "mul",
            Builtin::Div => "div",
            Builtin::Lt => "lt",
            Builtin::Eq => "eq",
            Builtin::Len => "len",
            Builtin::At => "at",
            Builtin::Slice => "slice",
            Builtin::Concat => "concat",
            Builtin::Push => "push",
            Builtin::Put => "put",
            Builtin::Code => "code",
            Builtin::Char => "char",
            Builtin::Kind => "kind",
            Builtin::String => "string",
            Builtin::Print => "print",
            Builtin::Fail => "fail",
        }
    }

    /// What it does to the stack, as help shows it.
    pub fn effect(self) -> &'static str {
        match self {
            Builtin::Drop => "x --",
            Builtin::Pick => "xn … x0 n -- xn … x0 xn",
            Builtin::Roll => "xn … x0 n -- xn-1 … x0 xn",
            Builtin::Add => "n m -- n+m",
            Builtin::Sub => "n m -- n-m",
            Builtin::Mul => "n m -- n*m",
            Builtin::Div => "n m -- n/m",
            Builtin::Lt => "a b -- a<b",
            Builtin::Eq => "a b -- a=b",
            Builtin::Len => "seq -- n",
            Builtin::At => "seq i -- x",
            Builtin::Slice => "seq from to -- seq",
            Builtin::Concat => "list -- seq",
            Builtin::Push => "seq x -- seq",
            Builtin::Put => "seq i x -- seq",
            Builtin::Code => "c -- n",
            Builtin::Char => "n -- c",
            Builtin::Kind => "x -- name",
            Builtin::String => "x -- s",
            Builtin::Print => "x --",
            Builtin::Fail => "x --",
        }
    }

    /// What it does, in a line, as help says it.
    pub fn about(self) -> &'static str {
        match self {
            Builtin::Drop => "the top value dropped",
            Builtin::Pick => "a copy of the value n under the top (0 pick copies the top)",
            Builtin::Roll => "the value n under the top moved to the top (1 roll swaps)",
            Builtin::Add | Builtin::Sub | Builtin::Mul => "integer arithmetic, checked",
            Builtin::Div => "integer division, checked; it truncates toward zero",
            Builtin::Lt => "whether a comes before b: two integers, two strings or two lists",
            Builtin::Eq => "whether two values are the same, of one kind",
            Builtin::Len => "how many elements a string or list has",
            Builtin::At => "element i, from 0 (of a string, a one-character string)",
            Builtin::Slice => "the elements from `from` up to before `to`",
            Builtin::Concat => {
                "a list of strings joined into one string, or a list of lists into one list"
            }
            Builtin::Push => "x added at the end (to a string, x is a string)",
            Builtin::Put => "element i replaced by x (in a string, by a string)",
            Builtin::Code => "the character code of a one-character string",
            Builtin::Char => "the one-character string with code n",
            Builtin::Kind => "integer, boolean, string, list, or the host's kind",
            Builtin::String => "x as text, as print shows it",
            Builtin::Print => "print x now, a line",
            Builtin::Fail => "stop the program, with x as the reason",
        }
    }

    /// How many values it takes: `pick` and `roll` take a count, then as
    /// many values as it says (the machine checks those).
    pub fn takes(self) -> usize {
        match self {
            Builtin::Drop
            | Builtin::Pick
            | Builtin::Roll
            | Builtin::Len
            | Builtin::Concat
            | Builtin::Code
            | Builtin::Char
            | Builtin::Kind
            | Builtin::String
            | Builtin::Print
            | Builtin::Fail => 1,
            Builtin::Add
            | Builtin::Sub
            | Builtin::Mul
            | Builtin::Div
            | Builtin::Lt
            | Builtin::Eq
            | Builtin::At
            | Builtin::Push => 2,
            Builtin::Slice | Builtin::Put => 3,
        }
    }
}

/// The words that read the token after them, or shape the program: no
/// definition or `let` may take their names.
pub const PARSING: [&str; 11] = [
    "str", "define", "end", "let", "if", "else", "then", "begin", "until", "list", "each",
];

/// The application's words: each a name, how many values it takes, and
/// how many it gives (a range, for a word that gives a value only
/// sometimes).
#[derive(Clone, Debug, Default)]
pub struct Vocabulary {
    words: Vec<(String, usize, RangeInclusive<usize>)>,
    index: HashMap<String, usize>,
}

impl Vocabulary {
    /// Adds a word taking `takes` values and giving `gives`; its id, which
    /// the host is called with. A name given twice keeps its first word.
    pub fn add(&mut self, name: &str, takes: usize, gives: RangeInclusive<usize>) -> usize {
        if let Some(id) = self.index.get(name) {
            return *id;
        }
        let id = self.words.len();
        self.words.push((name.to_owned(), takes, gives));
        self.index.insert(name.to_owned(), id);
        id
    }

    /// The word `id`'s name, how many values it takes, and how many it
    /// gives.
    pub fn word(&self, id: usize) -> Option<(&str, usize, RangeInclusive<usize>)> {
        self.words
            .get(id)
            .map(|(name, takes, gives)| (name.as_str(), *takes, gives.clone()))
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

    /// Whether it is short and straight enough to copy in at each call: a
    /// few steps of literals and built-ins, no jumps, calls or names.
    fn leaf(&self) -> bool {
        self.ops.len() <= 4
            && self.ops.iter().all(|op| match op {
                Op::Push(_) | Op::Builtin(_) | Op::PickN(_) | Op::RollN(_) => true,
                Op::App(_)
                | Op::Call(_)
                | Op::Jump(_)
                | Op::JumpUnless(_)
                | Op::SetLocal(_)
                | Op::GetLocal(_)
                | Op::SetGlobal(_)
                | Op::GetGlobal(_)
                | Op::Mark
                | Op::Gather
                | Op::EachStart
                | Op::EachNext(_) => false,
            })
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

/// What a name compiles to.
#[derive(Clone, Copy, Debug)]
enum Entry {
    Builtin(Builtin),
    App(usize),
    Word(usize),
    Global(usize),
}

/// A block open as compiling goes: where its jumps are, to patch.
enum Open {
    Define { at: Token },
    If { jump: usize, at: Token },
    Else { jump: usize, at: Token },
    Begin { start: usize, at: Token },
    List { at: Token },
    Each { head: usize, at: Token },
}

/// The definition being compiled: its index, its steps, and its own `let`
/// names.
struct Current<A> {
    id: usize,
    code: Steps<A>,
    locals: HashMap<Rc<str>, usize>,
}

/// Compiles programs: sources added in order, all into one, then finished.
/// A library's definitions first, then a script, say.
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
    /// How many definitions earlier sources made (the standard library's,
    /// say): only theirs are copied in at a call, so a program's own words
    /// stay calls, and its mistakes name them as written.
    earlier: usize,
}

impl<A: Clone> Compiler<A> {
    pub fn new(vocabulary: Vocabulary) -> Compiler<A> {
        let mut names = HashMap::new();
        for builtin in Builtin::ALL {
            names.insert(Rc::from(builtin.name()), Entry::Builtin(builtin));
        }
        for (id, (name, _, _)) in vocabulary.words.iter().enumerate() {
            names.insert(Rc::from(name.as_str()), Entry::App(id));
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
            earlier: 0,
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
        self.earlier = self.words.len();
        let mut tokens = tokens.iter();
        while let Some(token) = tokens.next() {
            let text = &*token.text;
            let literal = std::mem::replace(&mut self.literal, false);
            match text {
                "str" => {
                    let next = tokens.next().ok_or_else(|| missing(token, "a string"))?;
                    self.code()
                        .push(Op::Push(Value::Str(Rc::clone(&next.text))), token);
                }
                "define" => {
                    self.top_level(token)?;
                    let name = tokens.next().ok_or_else(|| missing(token, "a name"))?;
                    self.check_name(name, false)?;
                    let id = self.words.len();
                    // Visible from here on, its own body included.
                    self.names.insert(Rc::clone(&name.text), Entry::Word(id));
                    self.words.push(Steps::new());
                    self.current = Some(Current {
                        id,
                        code: Steps::new(),
                        locals: HashMap::new(),
                    });
                    self.open.push(Open::Define { at: token.clone() });
                }
                "end" => match self.open.pop() {
                    Some(Open::Define { .. }) => {
                        if let Some(current) = self.current.take()
                            && let Some(slot) = self.words.get_mut(current.id)
                        {
                            *slot = current.code;
                        }
                    }
                    Some(Open::List { .. }) => self.code().push(Op::Gather, token),
                    Some(Open::Each { head, .. }) => {
                        self.code().push(Op::Jump(head), token);
                        let out = self.code().len();
                        self.code().patch(head, out);
                    }
                    Some(other) => return Err(unclosed(&other)),
                    None => return Err(unmatched(token, "no define, list or each is open")),
                },
                "let" => {
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
                "if" => {
                    let jump = self.code().len();
                    self.code().push(Op::JumpUnless(0), token);
                    self.open.push(Open::If {
                        jump,
                        at: token.clone(),
                    });
                }
                "else" => match self.open.pop() {
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
                "then" => match self.open.pop() {
                    Some(Open::If { jump, .. } | Open::Else { jump, .. }) => {
                        let here = self.code().len();
                        self.code().patch(jump, here);
                    }
                    Some(other) => return Err(mismatched(token, &other)),
                    None => return Err(unmatched(token, "no if is open")),
                },
                "begin" => {
                    let start = self.code().len();
                    self.open.push(Open::Begin {
                        start,
                        at: token.clone(),
                    });
                }
                "until" => match self.open.pop() {
                    Some(Open::Begin { start, .. }) => {
                        self.code().push(Op::JumpUnless(start), token);
                    }
                    Some(other) => return Err(mismatched(token, &other)),
                    None => return Err(unmatched(token, "no begin is open")),
                },
                "list" => {
                    self.code().push(Op::Mark, token);
                    self.open.push(Open::List { at: token.clone() });
                }
                "each" => {
                    self.code().push(Op::EachStart, token);
                    let head = self.code().len();
                    self.code().push(Op::EachNext(0), token);
                    self.open.push(Open::Each {
                        head,
                        at: token.clone(),
                    });
                }
                text => self.word(token, text, literal)?,
            }
        }
        Ok(())
    }

    /// A word that is not a parsing word: a number, or a name.
    fn word(&mut self, token: &Token, text: &str, literal: bool) -> Result<(), Error> {
        if is_number(text) {
            let Ok(n) = text.parse::<i64>() else {
                return Err(Error {
                    code: Code::Overflow,
                    message: "a number past what an integer holds (64 bits)".to_owned(),
                    place: Some(token.place.clone()),
                    word: Some(Rc::clone(&token.text)),
                });
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
                let leaf = id < self.earlier;
                let body = self.words.get(id).filter(|w| leaf && w.leaf()).cloned();
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
                return Err(Error {
                    code: Code::UnknownWord,
                    message: "no such word".to_owned(),
                    place: Some(token.place.clone()),
                    word: Some(Rc::clone(&token.text)),
                });
            }
        }
        Ok(())
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
        crate::check::check(&program)?;
        Ok(program)
    }

    fn top_level(&self, token: &Token) -> Result<(), Error> {
        if self.open.is_empty() {
            return Ok(());
        }
        Err(Error {
            code: Code::NotTopLevel,
            message: "define goes at the top level, outside any definition or block".to_owned(),
            place: Some(token.place.clone()),
            word: Some(Rc::clone(&token.text)),
        })
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
        } else if PARSING.contains(&text) {
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
            Some(why) => Err(Error {
                code: Code::BadName,
                message: format!("{text:?} cannot be defined: it is {why}"),
                place: Some(name.place.clone()),
                word: Some(Rc::clone(&name.text)),
            }),
        }
    }
}

fn missing(token: &Token, what: &str) -> Error {
    Error {
        code: Code::MissingToken,
        message: format!("{} needs {what} after it", token.text),
        place: Some(token.place.clone()),
        word: Some(Rc::clone(&token.text)),
    }
}

fn unmatched(token: &Token, why: &str) -> Error {
    Error {
        code: Code::Unmatched,
        message: why.to_owned(),
        place: Some(token.place.clone()),
        word: Some(Rc::clone(&token.text)),
    }
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
    Error {
        code: Code::Unclosed,
        message: format!("this {what} is never closed"),
        place: Some(at.place.clone()),
        word: Some(Rc::clone(&at.text)),
    }
}

fn mismatched(token: &Token, open: &Open) -> Error {
    let (at, what) = opened(open);
    Error {
        code: Code::Unmatched,
        message: format!("the open block is {what}, from {}", at.place),
        place: Some(token.place.clone()),
        word: Some(Rc::clone(&token.text)),
    }
}

/// Whether `text` is written as an integer: digits, after a `-` or not.
/// Nothing else is a number: `+5` and `1e3` are names.
fn is_number(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}
