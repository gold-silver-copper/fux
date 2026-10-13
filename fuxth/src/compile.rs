//! Compiling: a program's tokens, all of them, into flat lists of steps,
//! before anything runs. Names are found as they are compiled, so a word no
//! one defines, a block left open or a stray `end` is an error before the
//! program does anything.
//!
//! The parsing words are a fixed set, the only ones that read the token
//! after them: `str`, `define NAME … end` and `constant NAME`. Control flow
//! (`if … else … then`, `begin … until`) is compiled to jumps, its open
//! blocks kept on a compile-time stack. Every other word takes and gives
//! values on the stack, and nothing else.
use crate::error::{Code, Error};
use crate::token::Token;
use crate::value::Value;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::rc::Rc;

/// The words fuxth itself gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Builtin {
    Dup,
    Drop,
    Swap,
    Over,
    Rot,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Lt,
    Gt,
    Not,
}

impl Builtin {
    pub const ALL: [Builtin; 14] = [
        Builtin::Dup,
        Builtin::Drop,
        Builtin::Swap,
        Builtin::Over,
        Builtin::Rot,
        Builtin::Add,
        Builtin::Sub,
        Builtin::Mul,
        Builtin::Div,
        Builtin::Mod,
        Builtin::Eq,
        Builtin::Lt,
        Builtin::Gt,
        Builtin::Not,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Builtin::Dup => "dup",
            Builtin::Drop => "drop",
            Builtin::Swap => "swap",
            Builtin::Over => "over",
            Builtin::Rot => "rot",
            Builtin::Add => "add",
            Builtin::Sub => "sub",
            Builtin::Mul => "mul",
            Builtin::Div => "div",
            Builtin::Mod => "mod",
            Builtin::Eq => "eq",
            Builtin::Lt => "lt",
            Builtin::Gt => "gt",
            Builtin::Not => "not",
        }
    }

    /// What it does to the stack, as help shows it.
    pub fn effect(self) -> &'static str {
        match self {
            Builtin::Dup => "a -- a a",
            Builtin::Drop => "a --",
            Builtin::Swap => "a b -- b a",
            Builtin::Over => "a b -- a b a",
            Builtin::Rot => "a b c -- b c a",
            Builtin::Add => "n m -- n+m",
            Builtin::Sub => "n m -- n-m",
            Builtin::Mul => "n m -- n*m",
            Builtin::Div => "n m -- n/m",
            Builtin::Mod => "n m -- remainder",
            Builtin::Eq => "a b -- a=b",
            Builtin::Lt => "n m -- n<m",
            Builtin::Gt => "n m -- n>m",
            Builtin::Not => "flag -- !flag",
        }
    }

    /// How many values it takes.
    pub fn takes(self) -> usize {
        match self {
            Builtin::Dup | Builtin::Drop | Builtin::Not => 1,
            Builtin::Swap
            | Builtin::Over
            | Builtin::Add
            | Builtin::Sub
            | Builtin::Mul
            | Builtin::Div
            | Builtin::Mod
            | Builtin::Eq
            | Builtin::Lt
            | Builtin::Gt => 2,
            Builtin::Rot => 3,
        }
    }
}

/// The words that read the token after them, or shape the program: no
/// definition may take their names.
pub const PARSING: [&str; 9] = [
    "str", "define", "end", "constant", "if", "else", "then", "begin", "until",
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
    App(usize),
    /// A definition, by its index.
    Call(usize),
    Jump(usize),
    /// Takes a boolean; jumps if it is false.
    JumpUnless(usize),
    SetConst(usize),
    GetConst(usize),
}

/// Steps, each with the token it came from.
#[derive(Clone, Debug)]
pub(crate) struct Steps<A> {
    pub ops: Vec<Op<A>>,
    pub tokens: Vec<Token>,
}

impl<A> Steps<A> {
    fn push(&mut self, op: Op<A>, token: &Token) {
        self.ops.push(op);
        self.tokens.push(token.clone());
    }
    fn len(&self) -> usize {
        self.ops.len()
    }
    /// Points the jump at `at` to `to`.
    fn patch(&mut self, at: usize, to: usize) {
        if let Some(Op::Jump(target) | Op::JumpUnless(target)) = self.ops.get_mut(at) {
            *target = to;
        }
    }
}

/// A compiled program: its top level, and its definitions.
#[derive(Clone, Debug)]
pub struct Program<A> {
    pub(crate) main: Steps<A>,
    pub(crate) words: Vec<Steps<A>>,
    pub(crate) constants: usize,
    pub(crate) vocabulary: Vocabulary,
}

/// What a name compiles to.
#[derive(Clone, Copy, Debug)]
enum Entry {
    Builtin(Builtin),
    App(usize),
    Word(usize),
    Const(usize),
}

/// A block open as compiling goes: where its jumps are, to patch.
enum Open {
    Define { at: Token },
    If { jump: usize, at: Token },
    Else { jump: usize, at: Token },
    Begin { start: usize, at: Token },
}

/// Compiles programs: sources added in order, all into one, then finished.
/// A library's definitions first, then a script, say.
pub struct Compiler<A> {
    vocabulary: Vocabulary,
    names: HashMap<Rc<str>, Entry>,
    main: Steps<A>,
    words: Vec<Steps<A>>,
    constants: usize,
    open: Vec<Open>,
    /// The definition being compiled, while one is.
    current: Option<(usize, Steps<A>)>,
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
            main: Steps {
                ops: Vec::new(),
                tokens: Vec::new(),
            },
            words: Vec::new(),
            constants: 0,
            open: Vec::new(),
            current: None,
        }
    }

    /// The code being compiled into: the definition open, else the top
    /// level.
    fn code(&mut self) -> &mut Steps<A> {
        match &mut self.current {
            Some((_, code)) => code,
            None => &mut self.main,
        }
    }

    /// Compiles `tokens` onto what was compiled before.
    pub fn add(&mut self, tokens: &[Token]) -> Result<(), Error> {
        let mut tokens = tokens.iter();
        while let Some(token) = tokens.next() {
            let text = &*token.text;
            match text {
                "str" => {
                    let next = tokens.next().ok_or_else(|| missing(token, "a string"))?;
                    self.code()
                        .push(Op::Push(Value::Str(Rc::clone(&next.text))), token);
                }
                "define" => {
                    self.top_level(token)?;
                    let name = tokens.next().ok_or_else(|| missing(token, "a name"))?;
                    self.check_name(name)?;
                    let id = self.words.len();
                    // Visible from here on, its own body included.
                    self.names.insert(Rc::clone(&name.text), Entry::Word(id));
                    self.words.push(Steps {
                        ops: Vec::new(),
                        tokens: Vec::new(),
                    });
                    self.current = Some((
                        id,
                        Steps {
                            ops: Vec::new(),
                            tokens: Vec::new(),
                        },
                    ));
                    self.open.push(Open::Define { at: token.clone() });
                }
                "end" => match self.open.pop() {
                    Some(Open::Define { .. }) => {
                        if let Some((id, code)) = self.current.take()
                            && let Some(slot) = self.words.get_mut(id)
                        {
                            *slot = code;
                        }
                    }
                    Some(other) => return Err(unclosed(&other)),
                    None => return Err(unmatched(token, "no define is open")),
                },
                "constant" => {
                    self.top_level(token)?;
                    let name = tokens.next().ok_or_else(|| missing(token, "a name"))?;
                    self.check_name(name)?;
                    let slot = self.constants;
                    self.constants = slot.saturating_add(1);
                    self.code().push(Op::SetConst(slot), token);
                    self.names.insert(Rc::clone(&name.text), Entry::Const(slot));
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
                text => {
                    let number = is_number(text).then(|| text.parse::<i64>());
                    let op = match (number, self.names.get(text)) {
                        (Some(Ok(n)), _) => Op::Push(Value::Int(n)),
                        // Digits, but more than an integer holds.
                        (Some(Err(_)), _) => {
                            return Err(Error {
                                code: Code::Overflow,
                                message: "a number past what an integer holds (64 bits)".to_owned(),
                                place: Some(token.place.clone()),
                                word: Some(Rc::clone(&token.text)),
                            });
                        }
                        (None, Some(Entry::Builtin(b))) => Op::Builtin(*b),
                        (None, Some(Entry::App(id))) => Op::App(*id),
                        (None, Some(Entry::Word(id))) => Op::Call(*id),
                        (None, Some(Entry::Const(slot))) => Op::GetConst(*slot),
                        (None, None) => {
                            return Err(Error {
                                code: Code::UnknownWord,
                                message: "no such word".to_owned(),
                                place: Some(token.place.clone()),
                                word: Some(Rc::clone(&token.text)),
                            });
                        }
                    };
                    self.code().push(op, token);
                }
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
            constants: self.constants,
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
            message: "define and constant go at the top level, outside any definition or block"
                .to_owned(),
            place: Some(token.place.clone()),
            word: Some(Rc::clone(&token.text)),
        })
    }

    fn check_name(&self, name: &Token) -> Result<(), Error> {
        let text = &*name.text;
        let why = if text.is_empty() {
            Some("an empty name")
        } else if is_number(text) {
            Some("a number")
        } else if PARSING.contains(&text) {
            Some("a word of the language")
        } else if self.names.contains_key(text) {
            Some("a word there already")
        } else {
            None
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
