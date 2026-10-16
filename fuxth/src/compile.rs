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

/// Expands `Variant "name" takes "effect" "about", ...` to the enum
/// `Builtin` of those variants, `Builtin::ALL` in that order, and `name`,
/// `takes`, `effect` and `about`, each a match giving that variant's column.
macro_rules! builtins {
    ($($variant:ident $name:literal $takes:literal $effect:literal $about:literal,)*) => {
        /// The words fuxth itself gives: its core. Everything else is the
        /// standard library, written in fuxth.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Builtin {
            $($variant,)*
        }

        impl Builtin {
            pub const ALL: [Builtin; [$($name),*].len()] = [$(Builtin::$variant),*];

            pub fn name(self) -> &'static str {
                match self {
                    $(Builtin::$variant => $name,)*
                }
            }

            /// How many values it takes: `pick` and `roll` take a count,
            /// then as many values as it says (the machine checks those).
            pub fn takes(self) -> usize {
                match self {
                    $(Builtin::$variant => $takes,)*
                }
            }

            /// What it does to the stack, as help shows it.
            pub fn effect(self) -> &'static str {
                match self {
                    $(Builtin::$variant => $effect,)*
                }
            }

            /// What it does, in a line, as help says it.
            pub fn about(self) -> &'static str {
                match self {
                    $(Builtin::$variant => $about,)*
                }
            }
        }
    };
}

builtins! {
    Drop "drop" 1 "x --" "the top value dropped",
    Pick "pick" 1 "xn … x0 n -- xn … x0 xn" "a copy of the value n under the top (0 pick copies the top)",
    Roll "roll" 1 "xn … x0 n -- xn-1 … x0 xn" "the value n under the top moved to the top (1 roll swaps)",
    Add "add" 2 "n m -- n+m" "integer arithmetic, checked",
    Sub "sub" 2 "n m -- n-m" "integer arithmetic, checked",
    Mul "mul" 2 "n m -- n*m" "integer arithmetic, checked",
    Div "div" 2 "n m -- n/m" "integer division, checked; it truncates toward zero",
    Lt "lt" 2 "a b -- a<b" "whether a comes before b: two integers, two strings or two lists",
    Eq "eq" 2 "a b -- a=b" "whether two values are the same, of one kind",
    Len "len" 1 "seq -- n" "how many elements a string or list has",
    At "at" 2 "seq i -- x" "element i, from 0 (of a string, a one-character string)",
    Slice "slice" 3 "seq from to -- seq" "the elements from `from` up to before `to`",
    Concat "concat" 1 "list -- seq" "a list of strings joined into one string, or a list of lists into one list",
    Push "push" 2 "seq x -- seq" "x added at the end (to a string, x is a string)",
    Put "put" 3 "seq i x -- seq" "element i replaced by x (in a string, by a string)",
    Code "code" 1 "c -- n" "the character code of a one-character string",
    Char "char" 1 "n -- c" "the one-character string with code n",
    Kind "kind" 1 "x -- name" "integer, boolean, string, list, or the host's kind",
    String "string" 1 "x -- s" "x as text, as print shows it",
    Print "print" 1 "x --" "print x now, a line",
    Fail "fail" 1 "x --" "stop the program, with x as the reason",
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

/// The definition being compiled: its index, its steps, and its own `let`
/// names.
#[derive(Clone)]
struct Current<A> {
    id: usize,
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
            match text {
                "str" => {
                    let next = tokens.next().ok_or_else(|| missing(token, "a string"))?;
                    self.code().push(Op::Push(Value::str(&*next.text)), token);
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
                    Some(other) => return Err(mismatched(token, &other)),
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
