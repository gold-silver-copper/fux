//! The outer interpreter: it reads a source a word at a time and runs each
//! word as it is read, or, while a definition is open, compiles it. A
//! source is a frame of the machine's, as a call is, so reading stops and
//! goes on as running does, and a source read inside another (`include`,
//! `evaluate`) is read to its end before the words after it.
//!
//! The words that shape a program are immediate: they run even while a
//! definition is compiled. `define NAME … end` compiles a word, findable
//! from its `end` on; a construct (`if … then`, `begin … until`, `list …
//! end`, `each … end`, `try … catch … end`) compiles into the definition
//! open, or, at the top level, into code of its own, run as soon as its
//! last construct closes. While compiling, each open construct is a mark
//! on the stack that only its own code's words resolve, and `end` closes
//! the innermost.
use crate::Code;
use crate::error::Error;
use crate::machine::{Builtin, Entry, Frame, Host, Machine, Op, Steps};
use crate::token::{self, Token};
use crate::value::{AppValue, Mark, Text, Value, Xt};
use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

/// A source being read: its words, the next one, and its file.
#[derive(Clone, Debug)]
pub(crate) struct Read {
    tokens: Rc<[Token]>,
    next: usize,
    pub(crate) path: Option<PathBuf>,
}

impl Read {
    /// Its next word, taken.
    fn take(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.next).cloned()?;
        self.next = self.next.saturating_add(1);
        Some(token)
    }

    /// The word it read last.
    pub(crate) fn last(&self) -> Option<Token> {
        self.tokens.get(self.next.checked_sub(1)?).cloned()
    }
}

/// A construct a mark holds open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Construct {
    Define,
    Code,
    If,
    Else,
    Begin,
    List,
    Each,
    Try,
    Catch,
}

impl Construct {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Construct::Define => "define … end",
            Construct::Code => "code … end",
            Construct::If => "if … then",
            Construct::Else => "else … then",
            Construct::Begin => "begin … until",
            Construct::List => "list … end",
            Construct::Each => "each … end",
            Construct::Try => "try … catch",
            Construct::Catch => "try … catch … end",
        }
    }
}

/// What code being compiled becomes.
#[derive(Clone, Debug)]
pub(crate) enum Kind {
    /// A definition of the name.
    Define(Rc<str>),
    /// An xt (`code … end`).
    Code,
    /// The top level's own, run once its constructs close.
    Temp,
}

/// Code being compiled: its steps and `let` names, what it becomes, which
/// it is, how many of its marks are open, whether words were compiled
/// before it began, whether its last step is a number or `str` written
/// just before (so a `pick` or `roll` after it may take it as its count,
/// or a `try` as its codes), and, in a library, where its `define` is.
#[derive(Clone, Debug)]
pub(crate) struct Building<A> {
    code: Steps<A>,
    locals: HashMap<Rc<str>, usize>,
    kind: Kind,
    id: u64,
    open: usize,
    was_compiling: bool,
    literal: bool,
    from: Option<usize>,
}

/// A host for a library, which defines words and runs none of its host's.
struct Library<A>(std::marker::PhantomData<A>);

impl<A: AppValue> Host for Library<A> {
    type Value = A;
    type Request = ();
    fn call(
        &mut self,
        _: usize,
        _: crate::Args<'_, A>,
        _: &mut Vec<Value<A>>,
    ) -> Result<crate::Done<()>, crate::Failure> {
        Err(crate::Failure::new(
            Code::Contract,
            "a library defines words: it runs none of its host's",
        ))
    }
    fn write(&mut self, _: &str) -> Result<(), crate::Failure> {
        Err(crate::Failure::new(
            Code::Contract,
            "a library defines words: it writes nothing",
        ))
    }
}

impl<A: AppValue> Machine<A> {
    /// Reads `tokens` next, a source (the file `path`, if one), before
    /// what it was reading: the program, given to a machine at rest.
    pub fn read(&mut self, tokens: Vec<Token>, path: Option<PathBuf>) {
        if let Some(path) = &path {
            self.loaded.insert(path.clone(), false);
        }
        self.frames.push(Frame::Read(Read {
            tokens: tokens.into(),
            next: 0,
            path,
        }));
    }

    /// Reads `text`, a library named `source`, to its end, and gives each
    /// of its definitions' help, which the comment on the line just above
    /// its `define` says: `# EFFECT : ABOUT`. A definition without it is
    /// refused, and the machine is as it was. A library is read on a
    /// machine at rest, and only defines words.
    pub fn library(&mut self, source: &str, text: &str) -> Result<Vec<Doc>, Error> {
        if !self.frames.is_empty() || !self.building.is_empty() {
            let why = "a library is read on a machine at rest";
            return Err(Error::new(Code::Contract, why, None));
        }
        let spans = token::spans(source, text)?;
        let tokens = spans.iter().map(|(token, _)| token.clone()).collect();
        let before = self.clone();
        self.made = Some(Vec::new());
        self.read(tokens, None);
        let ran = self.run(&mut Library(std::marker::PhantomData), u64::MAX);
        let made = self.made.take().unwrap_or_default();
        let docs = ran.and_then(|outcome| match outcome {
            crate::Outcome::Finished => made
                .iter()
                .map(|&(from, to)| Doc::of(text, &spans, from, to))
                .collect::<Result<Vec<Doc>, Error>>(),
            crate::Outcome::Paused | crate::Outcome::Suspended(()) => {
                Err(Error::new(Code::Contract, "a library stopped short", None))
            }
        });
        if docs.is_err() {
            *self = before;
        }
        docs
    }

    /// The next word of the source on top, read and run (or compiled);
    /// past its last, the source ends. What an application word it ran
    /// waits for, if it does.
    pub(crate) fn read_word<H: Host<Value = A>>(
        &mut self,
        host: &mut H,
    ) -> Result<Option<H::Request>, Error> {
        let Some(Frame::Read(read)) = self.frames.last_mut() else {
            return Ok(None);
        };
        let Some(token) = read.take() else {
            self.read_last = read.last();
            if let Some(Frame::Read(Read {
                path: Some(path), ..
            })) = self.frames.pop()
            {
                self.loaded.insert(path, true);
            }
            return Ok(None);
        };
        let text = &*token.text;
        if self.compiling
            && let Some(slot) = (self.building.last()).and_then(|b| b.locals.get(text).copied())
        {
            self.compile(Op::GetLocal(slot));
            return Ok(None);
        }
        if is_number(text) {
            let Ok(n) = text.parse::<i64>() else {
                let why = "a number past what an integer holds (64 bits)";
                return Err(Error::new(Code::Overflow, why, Some(&token)));
            };
            self.given(Value::Int(n), true);
            return Ok(None);
        }
        if let Some(Entry { xt, immediate }) = self.names.get(text) {
            let xt = Rc::clone(xt);
            if self.compiling && !immediate {
                self.compile_word(&xt)?;
                return Ok(None);
            }
            return self.invoke(xt, host);
        }
        // A word the host reads as a value: the string, given to its word.
        if let Some(id) = self.vocabulary.literal(text) {
            let text = Value::str(text);
            if self.compiling {
                self.compile(Op::Push(text));
                self.compile(Op::App(id));
                return Ok(None);
            }
            self.push(text);
            return self.app(id, host);
        }
        let why = "no such word; text is `str WORD`";
        Err(Error::new(Code::UnknownWord, why, Some(&token)))
    }

    /// The word read last, of the innermost source.
    fn reading(&self) -> Option<Token> {
        self.frames.iter().rev().find_map(|frame| match frame {
            Frame::Read(read) => read.last(),
            Frame::Code { .. } => None,
        })
    }

    /// The next word of the innermost source, for `word`, which takes
    /// `what`.
    pub(crate) fn next_word(&mut self, word: Builtin, what: &str) -> Result<Token, Error> {
        let read = self.frames.iter_mut().rev().find_map(|frame| match frame {
            Frame::Read(read) => Some(read),
            Frame::Code { .. } => None,
        });
        match read.and_then(Read::take) {
            Some(token) => Ok(token),
            None => Err(self.fail(
                Code::MissingToken,
                format!("{} needs {what} after it", word.name()),
            )),
        }
    }

    /// `op` compiled into the code being compiled, said at the word read.
    fn compile(&mut self, op: Op<A>) {
        let token = self.reading();
        if let Some(building) = self.building.last_mut() {
            building.code.push(op, token);
            building.literal = false;
        }
    }

    /// `value` compiled, while compiling, `literal` if it was written just
    /// before; else pushed.
    fn given(&mut self, value: Value<A>, literal: bool) {
        if !self.compiling {
            return self.push(value);
        }
        self.compile(Op::Push(value));
        if let Some(building) = self.building.last_mut() {
            building.literal = literal;
        }
    }

    /// A call to `xt` compiled: a one-step word's step copied in (`3 pick`
    /// one step, its count known).
    pub(crate) fn compile_word(&mut self, xt: &Rc<Steps<A>>) -> Result<(), Error> {
        let Some(building) = self.building.last_mut().filter(|_| self.compiling) else {
            let why = format!("{} is compiled: it goes in a definition", xt.name);
            return Err(self.fail(Code::NotTopLevel, why));
        };
        let op = match xt.leaf() {
            Some(Op::Builtin(b @ (Builtin::Pick | Builtin::Roll))) => {
                let count = match (building.literal, building.code.ops.last()) {
                    (true, Some(Op::Push(Value::Int(n)))) => usize::try_from(*n).ok(),
                    _ => None,
                };
                match count {
                    Some(n) => {
                        building.code.ops.pop();
                        building.code.tokens.pop();
                        if *b == Builtin::Pick {
                            Op::PickN(n)
                        } else {
                            Op::RollN(n)
                        }
                    }
                    None => Op::Builtin(*b),
                }
            }
            Some(op) => op.clone(),
            None => Op::Call(Rc::clone(xt)),
        };
        self.compile(op);
        Ok(())
    }

    /// Begins code that becomes `kind`, compiled from here on.
    fn begin(&mut self, kind: Kind, name: Rc<str>, from: Option<usize>) {
        self.begun = self.begun.saturating_add(1);
        self.building.push(Building {
            code: Steps::new(name),
            locals: HashMap::new(),
            kind,
            id: self.begun,
            open: 0,
            was_compiling: self.compiling,
            literal: false,
            from,
        });
        self.compiling = true;
    }

    /// `construct` opened at step `at` of the code being compiled, said at
    /// `token`: its mark pushed.
    fn mark(&mut self, construct: Construct, at: usize, token: Option<Token>) {
        let Some(building) = self.building.last_mut() else {
            return;
        };
        building.open = building.open.saturating_add(1);
        let of = building.id;
        let mark = Mark {
            construct,
            of,
            at,
            token,
        };
        self.stack.push(Value::Mark(Rc::new(mark)));
    }

    /// Opens `construct`, its steps `ops` compiled, in the code being
    /// compiled, or, at the top level, in code of its own. Its mark is at
    /// its last step, or where it begins.
    pub(crate) fn open(&mut self, construct: Construct, ops: &[Op<A>]) -> Result<(), Error> {
        if !self.compiling {
            self.begin(Kind::Temp, Rc::from(""), None);
        }
        for op in ops {
            self.compile(op.clone());
        }
        let len = self.building.last().map_or(0, |b| b.code.len());
        let at = len.saturating_sub(usize::from(!ops.is_empty()));
        let token = self.reading();
        self.mark(construct, at, token);
        Ok(())
    }

    /// The mark on top, taken: one of `constructs`, of the code being
    /// compiled; with none open, `none` says what is missing.
    fn close(&mut self, constructs: &[Construct], none: &str) -> Result<Rc<Mark>, Error> {
        let id = self.building.last().map(|b| b.id);
        let mark = match self.stack.last() {
            Some(Value::Mark(mark)) => Rc::clone(mark),
            _ => return Err(self.fail(Code::Unmatched, none)),
        };
        if Some(mark.of) != id {
            let why = format!("the open block is {}, of other code", mark.construct.name());
            return Err(self.fail(Code::Unmatched, why));
        }
        if !constructs.contains(&mark.construct) {
            let from =
                (mark.token.as_ref()).map_or(String::new(), |t| format!(", from {}", t.place));
            let why = format!("the open block is {}{from}", mark.construct.name());
            return Err(self.fail(Code::Unmatched, why));
        }
        self.stack.pop();
        if let Some(building) = self.building.last_mut() {
            building.open = building.open.saturating_sub(1);
        }
        Ok(mark)
    }

    /// After a construct closes: the top level's own code, none of its
    /// constructs open now, runs.
    fn closed(&mut self) {
        let done =
            (self.building.last()).is_some_and(|b| matches!(b.kind, Kind::Temp) && b.open == 0);
        if done && let Some(building) = self.building.pop() {
            self.compiling = building.was_compiling;
            self.enter(Rc::new(building.code));
        }
    }

    fn len(&self) -> usize {
        self.building.last().map_or(0, |b| b.code.len())
    }

    fn patch(&mut self, at: usize, to: usize) {
        if let Some(building) = self.building.last_mut() {
            building.code.patch(at, to);
        }
    }

    /// `else`.
    pub(crate) fn otherwise(&mut self) -> Result<(), Error> {
        let mark = self.close(&[Construct::If], "no if is open")?;
        let here = self.len();
        self.compile(Op::Jump(0));
        self.patch(mark.at, here.saturating_add(1));
        let token = self.reading();
        self.mark(Construct::Else, here, token);
        Ok(())
    }

    /// `then`.
    pub(crate) fn then(&mut self) -> Result<(), Error> {
        let mark = self.close(&[Construct::If, Construct::Else], "no if is open")?;
        let here = self.len();
        self.patch(mark.at, here);
        self.closed();
        Ok(())
    }

    /// `until`.
    pub(crate) fn until(&mut self) -> Result<(), Error> {
        let mark = self.close(&[Construct::Begin], "no begin is open")?;
        self.compile(Op::JumpUnless(mark.at));
        self.closed();
        Ok(())
    }

    /// `try`, its codes the string written just before it: compiled
    /// there, or, opening the top level's own code, pushed.
    pub(crate) fn try_body(&mut self) -> Result<(), Error> {
        let written = match self.building.last_mut().filter(|_| self.compiling) {
            Some(b) => match b.code.ops.last() {
                Some(Op::Push(Value::Str(text))) if b.literal => {
                    let text = Rc::clone(text);
                    b.code.ops.pop();
                    b.code.tokens.pop();
                    Some(text)
                }
                _ => None,
            },
            None => match self.stack.last() {
                Some(Value::Str(text)) => {
                    let text = Rc::clone(text);
                    self.stack.pop();
                    Some(text)
                }
                _ => None,
            },
        };
        let Some(text) = written else {
            let why = "takes the codes it catches as a string written just before it: \
                       str timeout try, str 'timeout closed' try";
            return Err(self.fail(Code::BadCodes, why));
        };
        let words = Rc::clone(&self.vocabulary);
        let codes = (text.split_whitespace()).map(|name| words.caught(name));
        let codes = codes.collect::<Result<Vec<Code>, String>>();
        let codes = codes.map_err(|why| self.fail(Code::BadCodes, why))?;
        if codes.is_empty() {
            return Err(self.fail(Code::BadCodes, "names no code to catch"));
        }
        self.open(Construct::Try, &[Op::Try(0, Rc::new(codes))])
    }

    /// `catch`: said at its `try`, as an open block is.
    pub(crate) fn catch(&mut self) -> Result<(), Error> {
        let mark = self.close(&[Construct::Try], "no try is open")?;
        let jump = self.len();
        self.compile(Op::Tried(0));
        self.patch(mark.at, jump.saturating_add(1));
        self.mark(Construct::Catch, jump, mark.token.clone());
        Ok(())
    }

    /// `end`.
    pub(crate) fn end(&mut self) -> Result<(), Error> {
        use Construct::{Catch, Code as Coded, Define, Each, List};
        let none = "no block that end closes is open";
        let mark = self.close(&[Define, Coded, List, Each, Catch], none)?;
        match mark.construct {
            List => self.compile(Op::Gather),
            Each => {
                self.compile(Op::Jump(mark.at));
                let out = self.len();
                self.patch(mark.at, out);
            }
            Catch => {
                let out = self.len();
                self.patch(mark.at, out);
            }
            Define | Coded => {
                self.finished();
                return Ok(());
            }
            Construct::If | Construct::Else | Construct::Begin | Construct::Try => {}
        }
        self.closed();
        Ok(())
    }

    /// A definition or `code … end` closed: the word defined, or its xt
    /// given.
    fn finished(&mut self) {
        let Some(building) = self.building.pop() else {
            return;
        };
        self.compiling = building.was_compiling;
        let mut code = building.code;
        code.locals = building.locals.len();
        let code = Rc::new(code);
        let Kind::Define(name) = building.kind else {
            return self.given(Value::Xt(Xt(code)), false);
        };
        let at = self.position();
        if let (Some(made), Some(from), Some(to)) = (&mut self.made, building.from, at) {
            made.push((from, to));
        }
        let entry = Entry {
            xt: code,
            immediate: false,
        };
        self.names.insert(Rc::clone(&name), entry);
        self.latest = Some(name);
    }

    /// Where the word read last is in a library's tokens.
    fn position(&self) -> Option<usize> {
        match self.frames.as_slice() {
            [Frame::Read(read)] => read.next.checked_sub(1),
            _ => None,
        }
    }

    /// `define NAME`.
    pub(crate) fn define(&mut self) -> Result<(), Error> {
        if self.compiling || !self.building.is_empty() {
            let why = "define goes at the top level, outside any definition or block";
            return Err(self.fail(Code::NotTopLevel, why));
        }
        let at = self.reading();
        let name = self.next_word(Builtin::Define, "a name")?;
        self.check_name(&name)?;
        let from = self.position().and_then(|p| p.checked_sub(1));
        self.begin(Kind::Define(Rc::clone(&name.text)), name.text, from);
        self.mark(Construct::Define, 0, at);
        Ok(())
    }

    /// `code`.
    pub(crate) fn code(&mut self) -> Result<(), Error> {
        let at = self.reading();
        self.begin(Kind::Code, Rc::from(""), None);
        self.mark(Construct::Code, 0, at);
        Ok(())
    }

    /// The top-level `let` name `name`'s slot, if it is one.
    fn value_of(&self, name: &str) -> Option<usize> {
        match self.names.get(name).and_then(|entry| entry.xt.leaf()) {
            Some(Op::GetGlobal(slot)) => Some(*slot),
            _ => None,
        }
    }

    /// The local `name` of the code being compiled, if it is one: not the
    /// top level's own, whose names are top-level ones.
    fn compiling_locals(&mut self) -> Option<&mut HashMap<Rc<str>, usize>> {
        let building = self.building.last_mut().filter(|_| self.compiling)?;
        match building.kind {
            Kind::Define(_) | Kind::Code => Some(&mut building.locals),
            Kind::Temp => None,
        }
    }

    /// `let NAME`.
    pub(crate) fn let_name(&mut self) -> Result<(), Error> {
        let name = self.next_word(Builtin::Let, "a name")?;
        self.check_name(&name)?;
        if let Some(locals) = self.compiling_locals() {
            let next = locals.len();
            let slot = *locals.entry(Rc::clone(&name.text)).or_insert(next);
            self.compile(Op::SetLocal(slot));
            return Ok(());
        }
        let value = match self.compiling {
            true => None,
            false => Some(self.pop()?),
        };
        let slot = match self.value_of(&name.text) {
            Some(slot) => slot,
            None => {
                let slot = self.globals.len();
                self.globals.push(None);
                let xt = Steps::one(&name.text, Op::GetGlobal(slot));
                let entry = Entry {
                    xt,
                    immediate: false,
                };
                self.names.insert(name.text, entry);
                slot
            }
        };
        match value {
            Some(value) => {
                if let Some(global) = self.globals.get_mut(slot) {
                    *global = Some(value);
                }
            }
            None => self.compile(Op::SetGlobal(slot)),
        }
        Ok(())
    }

    /// `to NAME`.
    pub(crate) fn set_name(&mut self) -> Result<(), Error> {
        let name = self.next_word(Builtin::To, "a let name")?;
        if let Some(slot) = self
            .compiling_locals()
            .and_then(|l| l.get(&name.text).copied())
        {
            self.compile(Op::SetLocal(slot));
            return Ok(());
        }
        let Some(slot) = self.value_of(&name.text) else {
            let why = "to sets a let name, and this is none";
            return Err(Error::new(Code::BadName, why, Some(&name)));
        };
        if self.compiling {
            self.compile(Op::SetGlobal(slot));
            return Ok(());
        }
        let value = self.pop()?;
        if let Some(global) = self.globals.get_mut(slot) {
            *global = Some(value);
        }
        Ok(())
    }

    /// `str WORD`.
    pub(crate) fn string(&mut self) -> Result<(), Error> {
        let word = self.next_word(Builtin::Str, "a string")?;
        self.given(Value::Str(Rc::new(Text::new(&*word.text))), true);
        Ok(())
    }

    /// The word `name` names now.
    fn found(&self, name: &Token) -> Result<Entry<A>, Error> {
        match self.names.get(&*name.text) {
            Some(entry) => Ok(entry.clone()),
            None => {
                let why = "no such word; text is `str WORD`";
                Err(Error::new(Code::UnknownWord, why, Some(name)))
            }
        }
    }

    /// `tick NAME`.
    pub(crate) fn tick(&mut self) -> Result<(), Error> {
        let name = self.next_word(Builtin::Tick, "a name")?;
        let entry = self.found(&name)?;
        self.given(Value::Xt(Xt(entry.xt)), false);
        Ok(())
    }

    /// Refused unless a definition is being compiled, for `word`.
    fn compiled_only(&self, word: Builtin) -> Result<(), Error> {
        match self.compiling {
            true => Ok(()),
            false => {
                let why = format!("{} goes in a definition", word.name());
                Err(self.fail(Code::NotTopLevel, why))
            }
        }
    }

    /// `postpone NAME`.
    pub(crate) fn postpone(&mut self) -> Result<(), Error> {
        self.compiled_only(Builtin::Postpone)?;
        let name = self.next_word(Builtin::Postpone, "a name")?;
        let entry = self.found(&name)?;
        match entry.immediate {
            true => self.compile_word(&entry.xt),
            false => {
                self.compile(Op::Compile(entry.xt));
                Ok(())
            }
        }
    }

    /// `literal`.
    pub(crate) fn literal(&mut self, value: Value<A>) -> Result<(), Error> {
        self.compiled_only(Builtin::Literal)?;
        self.compile(Op::Push(value));
        Ok(())
    }

    /// `word`, compiled as `op`, in a definition or `code … end` only.
    pub(crate) fn compile_only(&mut self, word: Builtin, op: Op<A>) -> Result<(), Error> {
        if self.compiling_locals().is_none() {
            let why = format!("{} goes in a definition", word.name());
            return Err(self.fail(Code::NotTopLevel, why));
        }
        self.compile(op);
        Ok(())
    }

    /// `find`.
    pub(crate) fn find(&mut self, name: Rc<Text>) {
        match self.names.get(&**name) {
            Some(entry) => {
                let xt = Value::Xt(Xt(Rc::clone(&entry.xt)));
                self.push(xt);
                self.push(Value::Bool(true));
            }
            None => {
                self.push(Value::Str(name));
                self.push(Value::Bool(false));
            }
        }
    }

    /// `immediate`.
    pub(crate) fn immediate(&mut self) -> Result<(), Error> {
        let entry = (self.latest.as_ref()).and_then(|name| self.names.get_mut(name));
        match entry {
            Some(entry) => {
                entry.immediate = true;
                Ok(())
            }
            None => Err(self.fail(Code::BadName, "no word was defined yet")),
        }
    }

    /// `start-compiling`.
    pub(crate) fn start_compiling(&mut self) -> Result<(), Error> {
        if self.building.is_empty() {
            let why = "start-compiling compiles into a definition, and none is open";
            return Err(self.fail(Code::NotTopLevel, why));
        }
        self.compiling = true;
        Ok(())
    }

    /// `evaluate`: `text` read next, as a file is.
    pub(crate) fn evaluate(&mut self, text: &str) -> Result<(), Error> {
        let tokens = token::source("evaluate", text)?;
        self.read(tokens, None);
        Ok(())
    }

    /// `include PATH`, and `require PATH` (`once`): the file read next,
    /// through the host, relative to the file being read; required, a
    /// file read already is not read again, and one being read is a cycle.
    pub(crate) fn include<H: Host<Value = A>>(
        &mut self,
        host: &mut H,
        once: bool,
    ) -> Result<(), Error> {
        let word = if once {
            Builtin::Require
        } else {
            Builtin::Include
        };
        let at = self.reading();
        let path = self.next_word(word, "a path")?;
        let failed = |why| Error::new(Code::Require, format!("{}: {why}", path.text), at.as_ref());
        let from = self.frames.iter().rev().find_map(|frame| match frame {
            Frame::Read(read) => read.path.as_deref(),
            Frame::Code { .. } => None,
        });
        let canonical = host.resolve(&path.text, from).map_err(failed)?;
        match self.loaded.get(&canonical) {
            Some(true) if once => return Ok(()),
            Some(false) if once => {
                let why = format!("require cycle reaches {}", canonical.display());
                return Err(failed(why));
            }
            Some(_) | None => {}
        }
        let text = host.read(&canonical).map_err(failed)?;
        let place = at.map_or_else(|| path.place.to_string(), |t| t.place.to_string());
        let name = format!("{place} -> {}", canonical.display());
        let tokens = token::source(&name, &text)?;
        self.read(tokens, Some(canonical));
        Ok(())
    }

    /// Code left open at the end, said at its innermost mark.
    pub(crate) fn unclosed(&self) -> Error {
        let mark = self.stack.iter().rev().find_map(|value| match value {
            Value::Mark(mark) => Some(mark),
            Value::Int(_)
            | Value::Bool(_)
            | Value::Str(_)
            | Value::List(_)
            | Value::App(_)
            | Value::Xt(_) => None,
        });
        match mark {
            Some(mark) => Error::new(
                Code::Unclosed,
                format!("this {} is never closed", mark.construct.name()),
                mark.token.as_ref(),
            ),
            None => Error::new(Code::Unclosed, "a definition is left open", None),
        }
    }

    /// Whether `name` may be defined (`define`) or named (`let`): any name
    /// but a number, one the host reads as a value, and the core's words.
    fn check_name(&self, name: &Token) -> Result<(), Error> {
        let text = &*name.text;
        let why = if text.is_empty() {
            Some("an empty name".to_owned())
        } else if is_number(text) {
            Some("a number".to_owned())
        } else if let Some(id) = self.vocabulary.literal(text) {
            let word = self.vocabulary.names().nth(id).unwrap_or_default();
            Some(format!("read as a value: {text} is `str {text} {word}`"))
        } else if Builtin::named(text).is_some() {
            Some("a word of the core".to_owned())
        } else {
            None
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

/// Whether `text` is written as an integer: digits, after a `-` or not.
/// Nothing else is a number: `+5` and `1e3` are names.
fn is_number(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}
