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
use crate::token::{self, Source, Token, Words};
use crate::value::{Address, AppValue, Mark, Text, Value, Xt};
use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;

/// A source being read: its words, the next one, where the words a
/// parsing word read last begin (`next_word`, all of them up to `next`
/// while no other was read), its file, and which source given the
/// machine it is.
#[derive(Clone, Debug)]
pub(crate) struct Read {
    tokens: Rc<[Token]>,
    pub(crate) next: usize,
    parsed: Range<usize>,
    pub(crate) path: Option<PathBuf>,
    pub(crate) id: u64,
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
    While,
    Do,
    Case,
    Of,
    Endof,
    Ahead,
    List,
    Each,
    Try,
    Catch,
    Does,
}

impl Construct {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Construct::Define => "define … end",
            Construct::Code => "code … end",
            Construct::If => "if … then",
            Construct::Else => "else … then",
            Construct::Begin => "begin … until",
            Construct::While => "begin … while … end",
            Construct::Do => "do … loop",
            Construct::Case | Construct::Endof => "case … end",
            Construct::Of => "of … end",
            Construct::Ahead => "ahead … then",
            Construct::List => "list … end",
            Construct::Each => "each … end",
            Construct::Try => "try … catch",
            Construct::Catch => "try … catch … end",
            Construct::Does => "does … end",
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
    /// What follows `does` in a definition.
    Does,
    /// The top level's own, run once its constructs close.
    Temp,
}

/// Code being compiled: its steps and `let` names, what it becomes, which
/// it is, how many of its marks are open, whether words were compiled
/// before it began, whether its last step is a number or `str` written
/// just before (so a `pick` or `roll` after it may take it as its count,
/// or a `try` as its codes), in a library, where its `define` is, and, a
/// `code … end`'s, the source it is read from (by id) and where in it;
/// a definition's, the words it is made of, gathered as they are read.
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
    words: Option<(u64, usize)>,
    made: Option<Gathered>,
}

/// The words a definition is made of, gathered from the sources that give
/// them: the runs read already, and the run being read, if a source is.
#[derive(Clone, Debug)]
struct Gathered {
    runs: Vec<Words>,
    run: Option<Run>,
}

/// A run of a source's words being read: the source's frame, id and
/// tokens, and the word the run begins at.
#[derive(Clone, Debug)]
struct Run {
    at: usize,
    id: u64,
    tokens: Rc<[Token]>,
    from: usize,
}

impl Run {
    /// Where its source is read up to, if it is still read, in `frames`.
    fn next(&self, frames: &[Frame]) -> Option<usize> {
        match frames.get(self.at) {
            Some(Frame::Read(read)) if read.id == self.id => Some(read.next),
            Some(Frame::Read(_) | Frame::Code { .. }) | None => None,
        }
    }
}

impl Gathered {
    /// The run, its source read up to `to`, closed.
    fn close(&mut self, to: usize) {
        let run = self.run.take();
        let words = run.and_then(|run| Words::of(&run.tokens, run.from..to));
        self.runs
            .extend(words.filter(|words| !words.tokens().is_empty()));
    }
}

impl<A: AppValue> Machine<A> {
    /// Reads `tokens` next, a source (the file `path`, if one), before
    /// what it was reading: the program, given to a machine at rest.
    pub fn read(&mut self, tokens: Vec<Token>, path: Option<PathBuf>) {
        if let Some(path) = &path {
            self.loaded.insert(path.clone(), false);
        }
        self.sources = self.sources.saturating_add(1);
        let tokens: Rc<[Token]> = tokens.into();
        let at = self.frames.len();
        let run = || Run {
            at,
            id: self.sources,
            tokens: Rc::clone(&tokens),
            from: 0,
        };
        let Some(made) = self.building.first_mut().and_then(|b| b.made.as_mut()) else {
            return self.push_read(tokens, path);
        };
        // A definition open goes on in this source, unless a word of its
        // own (`include`) opened it, read from the source it is read from.
        let reading = (made.run.as_ref()).and_then(|run| Some((run.next(&self.frames)?, run.from)));
        match reading {
            Some((next, from)) if next > from => {}
            Some((next, _)) => {
                made.close(next);
                made.run = Some(run());
            }
            None => made.run = Some(run()),
        }
        self.push_read(tokens, path);
    }

    fn push_read(&mut self, tokens: Rc<[Token]>, path: Option<PathBuf>) {
        self.frames.push(Frame::Read(Box::new(Read {
            tokens,
            next: 0,
            parsed: 0..0,
            path,
            id: self.sources,
        })));
    }

    /// `read`, a source, read to its end or left: the run of the open
    /// definition's words read from it closed, and one begun in the source
    /// under it, which it may go on in.
    pub(crate) fn source_left(&mut self, read: &Read) {
        let Machine {
            building, frames, ..
        } = self;
        let Some(made) = building.first_mut().and_then(|b| b.made.as_mut()) else {
            return;
        };
        if made.run.as_ref().is_none_or(|run| run.id != read.id) {
            return;
        }
        made.close(read.next);
        let under = frames
            .iter()
            .enumerate()
            .rev()
            .find_map(|(at, frame)| match frame {
                Frame::Read(read) => Some((at, read)),
                Frame::Code { .. } => None,
            });
        made.run = under.map(|(at, read)| Run {
            at,
            id: read.id,
            tokens: Rc::clone(&read.tokens),
            from: read.next,
        });
    }

    /// Reads `text`, a library named `source`, to its end, and gives each
    /// of its definitions' help, which the comment on the line just above
    /// its `define` says: `# EFFECT : ABOUT`; the dictionary keeps it with
    /// the definition (`Definition`), and a definition the library hid
    /// (`hide`) gives none. A definition without it is refused,
    /// and the machine is as it was. A library is read on a machine at
    /// rest, run as any program is, on `host` (the one its programs run on,
    /// so the machine's code is made for one host), and may not wait for it.
    pub fn library<H: Host<Value = A>>(
        &mut self,
        source: &str,
        text: &str,
        host: &mut H,
    ) -> Result<Vec<Rc<Doc>>, Error> {
        if !self.frames.is_empty() || !self.building.is_empty() {
            let why = "a library is read on a machine at rest";
            return Err(Error::new(Code::Contract, why, None));
        }
        let spans = token::spans(source, text)?;
        let tokens = spans.iter().map(|(token, _)| token.clone()).collect();
        let before = self.clone();
        self.made = Some(Vec::new());
        self.read(tokens, None);
        let ran = self.run(host, u64::MAX);
        let made = self.made.take().unwrap_or_default();
        let docs = ran.and_then(|outcome| match outcome {
            crate::Outcome::Finished => made
                .iter()
                .map(|&(from, to)| Doc::of(text, &spans, from, to))
                .collect::<Result<Vec<Doc>, Error>>(),
            crate::Outcome::Paused | crate::Outcome::Suspended(_) => {
                Err(Error::new(Code::Contract, "a library stopped short", None))
            }
        });
        match docs {
            Ok(docs) => {
                let mut docs: Vec<Rc<Doc>> = docs.into_iter().map(Rc::new).collect();
                docs.retain(|doc| {
                    let entry = self.names.get_mut(doc.name.as_str());
                    entry
                        .map(|entry| entry.doc = Some(Rc::clone(doc)))
                        .is_some()
                });
                Ok(docs)
            }
            Err(error) => {
                *self = before;
                Err(error)
            }
        }
    }

    /// The next word of the source on top, read and run (or compiled);
    /// past its last, the source ends. What an application word it ran
    /// waits for, if it does.
    #[inline(never)]
    pub(crate) fn read_word<H: Host<Value = A>>(
        &mut self,
        host: &mut H,
    ) -> Result<Option<H::Request>, Error> {
        let Some(Frame::Read(read)) = self.frames.last_mut() else {
            return Ok(None);
        };
        let Some(token) = read.take() else {
            self.read_last = read.last();
            if let Some(Frame::Read(read)) = self.frames.pop() {
                self.source_left(&read);
                if let Some(path) = read.path {
                    self.loaded.insert(path, true);
                }
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
        if let Some(Entry { xt, .. }) = self.names.get(text) {
            // A word of the core's that shapes the program runs here, in
            // no frame of its own.
            let immediate = xt.immediate;
            if let (true, Some(Op::Builtin(word))) = (immediate, xt.leaf()) {
                // The core word itself, not a word of one step made of it.
                self.read_here = *xt.name == *word.name();
                let word = *word;
                let ran = self.builtin(word, host);
                self.read_here = false;
                return ran.map(|()| None);
            }
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
        let taken = read.and_then(|read| {
            if read.parsed.end != read.next {
                read.parsed.start = read.next;
            }
            let token = read.take()?;
            read.parsed.end = read.next;
            Some(token)
        });
        match taken {
            Some(token) => Ok(token),
            None => Err(self.fail(
                Code::MissingToken,
                format!("{} needs {what} after it", word.name()),
            )),
        }
    }

    /// The word of its own the reader read last, of the innermost source,
    /// to run or compile it: the source's id, and where it is there. It is
    /// the one before the words parsing words read since, if any were.
    fn invoked(&self) -> Option<(u64, usize)> {
        let read = self.source()?;
        let after = match read.parsed.end == read.next {
            true => read.parsed.start,
            false => read.next,
        };
        Some((read.id, after.checked_sub(1)?))
    }

    /// The source being read, the innermost.
    fn source(&self) -> Option<&Read> {
        self.frames.iter().rev().find_map(|frame| match frame {
            Frame::Read(read) => Some(&**read),
            Frame::Code { .. } => None,
        })
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
    #[inline(never)]
    pub(crate) fn compile_word(&mut self, xt: &Rc<Steps<A>>) -> Result<(), Error> {
        if !self.compiling || self.building.is_empty() {
            let why = format!("{} is compiled: it goes in a definition", xt.name);
            return Err(self.fail(Code::NotTopLevel, why));
        }
        self.compile_call(xt)
    }

    /// `compile-call`: a call to `xt` compiled into the code open, though
    /// its words are run (`stop-compiling`), as `compile_word` compiles it.
    pub(crate) fn compile_call(&mut self, xt: &Rc<Steps<A>>) -> Result<(), Error> {
        if self.building.is_empty() {
            let why = "compile-call compiles into a definition, and none is open";
            return Err(self.fail(Code::NotTopLevel, why));
        }
        let call = match xt.leaf() {
            Some(_) => None,
            None => Some(self.index_of(xt)?),
        };
        let Some(building) = self.building.last_mut() else {
            return Ok(());
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
            None => Op::Call(call.unwrap_or(xt.index), xt.locals),
        };
        self.compile(op);
        Ok(())
    }

    /// Begins code that becomes `kind`, compiled from here on.
    fn begin(&mut self, kind: Kind, name: Rc<str>, from: Option<usize>) {
        self.begun = self.begun.saturating_add(1);
        let words = match kind {
            Kind::Code => (self.source())
                .filter(|_| self.read_here)
                .map(|read| (read.id, read.next)),
            Kind::Define(_) | Kind::Does | Kind::Temp => None,
        };
        // A definition is made of the words from the one the reader read
        // and ran that began it (`define`, or a word that runs it).
        let made = match kind {
            // A library's own are known from where they are (`finished`).
            Kind::Define(_) if self.made.is_some() && from.is_some() => None,
            Kind::Define(_) => self.invoked().and_then(|(id, from)| {
                let at = (self.frames.iter())
                    .rposition(|frame| matches!(frame, Frame::Read(read) if read.id == id))?;
                let Some(Frame::Read(read)) = self.frames.get(at) else {
                    return None;
                };
                let tokens = Rc::clone(&read.tokens);
                let run = Some(Run {
                    at,
                    id,
                    tokens,
                    from,
                });
                Some(Gathered {
                    runs: Vec::new(),
                    run,
                })
            }),
            Kind::Code | Kind::Does | Kind::Temp => None,
        };
        self.building.push(Building {
            code: Steps::new(name),
            locals: HashMap::new(),
            kind,
            id: self.begun,
            open: 0,
            was_compiling: self.compiling,
            literal: false,
            from,
            words,
            made,
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
        // Where a construct begins is where a jump lands: a number written
        // before it is no count of the word after it.
        building.literal = false;
        let of = building.id;
        let mark = Mark {
            construct,
            of,
            at,
            token,
        };
        self.stack.push(Value::Mark(Rc::new(mark)));
    }

    /// Opens `construct` (`if`, `begin`, `list`, `each`) in the code being
    /// compiled, or, at the top level, in code of its own: its first steps
    /// compiled, and its mark at its last, or where it begins.
    pub(crate) fn open(&mut self, construct: Construct) -> Result<(), Error> {
        let ops = match construct {
            Construct::If => vec![Op::JumpUnless(0)],
            Construct::List => vec![Op::ListStart],
            Construct::Each => vec![Op::EachStart, Op::EachNext(0)],
            Construct::Do => vec![Op::Do(0)],
            Construct::Ahead => vec![Op::Jump(0)],
            Construct::Begin
            | Construct::Case
            | Construct::Define
            | Construct::Code
            | Construct::Else
            | Construct::While
            | Construct::Of
            | Construct::Endof
            | Construct::Try
            | Construct::Catch
            | Construct::Does => Vec::new(),
        };
        self.opened(construct, ops)
    }

    /// `construct` opened as `open` opens it, with its steps `ops`.
    fn opened(&mut self, construct: Construct, ops: Vec<Op<A>>) -> Result<(), Error> {
        if !self.compiling {
            self.begin(Kind::Temp, Rc::from(""), None);
        }
        let some = !ops.is_empty();
        for op in ops {
            self.compile(op);
        }
        let len = self.building.last().map_or(0, |b| b.code.len());
        let at = len.saturating_sub(usize::from(some));
        let token = self.reading();
        self.mark(construct, at, token);
        Ok(())
    }

    /// The mark on top, taken: one of `constructs`, of the code being
    /// compiled; with none open, `none` says what is missing.
    fn close(&mut self, constructs: &[Construct], none: &str) -> Result<Rc<Mark>, Error> {
        let mark = self.top(constructs, none)?;
        self.stack.pop();
        if let Some(building) = self.building.last_mut() {
            building.open = building.open.saturating_sub(1);
            // Where it closes is where a jump lands, as where it opens is.
            building.literal = false;
        }
        Ok(mark)
    }

    /// The mark on top, left there: one of `constructs`, of the code being
    /// compiled; with none open, `none` says what is missing.
    fn top(&self, constructs: &[Construct], none: &str) -> Result<Rc<Mark>, Error> {
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
        Ok(mark)
    }

    /// After a construct closes: the top level's own code, none of its
    /// constructs open now, runs.
    fn closed(&mut self) -> Result<(), Error> {
        let done =
            (self.building.last()).is_some_and(|b| matches!(b.kind, Kind::Temp) && b.open == 0);
        if done && let Some(building) = self.building.pop() {
            self.compiling = building.was_compiling;
            let code = self.version(building.code);
            return self.enter(&code);
        }
        Ok(())
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
        use Construct::{Ahead, Else, If};
        let mark = self.close(&[If, Else, Ahead], "no if or ahead is open")?;
        let here = self.len();
        self.patch(mark.at, here);
        self.closed()
    }

    /// `until`.
    pub(crate) fn until(&mut self) -> Result<(), Error> {
        let mark = self.close(&[Construct::Begin], "no begin is open")?;
        self.compile(Op::JumpUnless(mark.at));
        self.closed()
    }

    /// `while`: its test compiled, its mark over its `begin`'s.
    pub(crate) fn test_loop(&mut self) -> Result<(), Error> {
        self.top(&[Construct::Begin], "no begin is open")?;
        let at = self.len();
        self.compile(Op::JumpUnless(0));
        let token = self.reading();
        self.mark(Construct::While, at, token);
        Ok(())
    }

    /// `again`.
    pub(crate) fn again(&mut self) -> Result<(), Error> {
        let mark = self.close(&[Construct::Begin], "no begin is open")?;
        self.compile(Op::Jump(mark.at));
        self.closed()
    }

    /// `loop` and `loop-by`, `step` the step that ends each time round,
    /// given where the body begins.
    pub(crate) fn close_loop(&mut self, step: fn(usize) -> Op<A>) -> Result<(), Error> {
        let mark = self.close(&[Construct::Do], "no do is open")?;
        self.compile(step(mark.at.saturating_add(1)));
        let out = self.len();
        self.patch(mark.at, out);
        self.closed()
    }

    /// `of`: a test of the value under it, compiled.
    pub(crate) fn of(&mut self) -> Result<(), Error> {
        self.top(&[Construct::Case, Construct::Endof], "no case is open")?;
        self.compile(Op::PickN(1));
        self.compile(Op::Builtin(Builtin::Eq));
        let at = self.len();
        self.compile(Op::JumpUnless(0));
        self.compile(Op::Builtin(Builtin::Drop));
        let token = self.reading();
        self.mark(Construct::Of, at, token);
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
        let codes = codes.collect::<Result<Vec<Vec<Code>>, String>>();
        let codes = codes
            .map_err(|why| self.fail(Code::BadCodes, why))?
            .concat();
        if codes.is_empty() {
            return Err(self.fail(Code::BadCodes, "names no code to catch"));
        }
        self.opened(Construct::Try, vec![Op::Try(0, Rc::new(codes))])
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
        use Construct::{Case, Catch, Code as Coded, Define, Does, Each, Endof, List, Of, While};
        let none = "no block that end closes is open";
        let closes = [
            Define, Coded, Does, List, Each, Catch, While, Of, Case, Endof,
        ];
        let mut mark = self.close(&closes, none)?;
        // What follows each `does` closes with its definition.
        while mark.construct == Does {
            self.end_does(&mark);
            mark = self.close(&[Define, Coded, Does], none)?;
        }
        match mark.construct {
            List => self.compile(Op::Gather),
            While => {
                let begin = self.close(&[Construct::Begin], "no begin is open")?;
                self.compile(Op::Jump(begin.at));
                let out = self.len();
                self.patch(mark.at, out);
            }
            // The case goes on: a jump past its end, which the case's
            // `end` points.
            Of => {
                let jump = self.len();
                self.compile(Op::Jump(0));
                self.patch(mark.at, jump.saturating_add(1));
                self.mark(Endof, jump, mark.token.clone());
                return Ok(());
            }
            Case | Endof => {
                self.compile(Op::Builtin(Builtin::Drop));
                let out = self.len();
                let mut mark = mark;
                while mark.construct == Endof {
                    self.patch(mark.at, out);
                    mark = self.close(&[Endof, Case], "no case is open")?;
                }
            }
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
            Construct::If
            | Construct::Else
            | Construct::Begin
            | Construct::Do
            | Construct::Ahead
            | Construct::Try
            | Does => {}
        }
        self.closed()
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
        let Kind::Define(name) = building.kind else {
            // The words read from its source between the `code` and the
            // `end` the reader itself read there and ran, so `code` WORDS
            // `end` read again does as they did; none if another word
            // opened or closed it.
            code.words = (building.words)
                .filter(|_| self.read_here)
                .and_then(|(id, from)| self.read_since(id, from, 1));
            let code = self.version(code);
            return self.given(Value::Xt(Xt(code)), false);
        };
        let code = self.version(code);
        let at = self.position();
        let mut source = None;
        if let (Some(made), Some(from), Some(to)) = (&mut self.made, building.from, at) {
            made.push((from, to));
            // A library's own: its words from its `define` to its `end`,
            // where they are in the one source read, the first frame's,
            // which gathered none.
            let words = match self.frames.first() {
                Some(Frame::Read(read)) => Words::of(&read.tokens, from..to.saturating_add(1)),
                Some(Frame::Code { .. }) | None => None,
            };
            source = words.map(|words| Source::of(Vec::new(), words));
        }
        // Its words gathered: none known if a source it was read from
        // couldn't say.
        if let Some(made) = building.made {
            source = made.run.and_then(|run| {
                let last = Words::of(&run.tokens, run.from..run.next(&self.frames)?)?;
                Some(Source::of(made.runs, last))
            });
        }
        self.define_word(Rc::clone(&name), code, source);
        self.latest = Some(name);
    }

    /// The words of the source `id`, the innermost being read, from `from`
    /// up to the word read last, but for the `short` last of them.
    fn read_since(&self, id: u64, from: usize, short: usize) -> Option<Words> {
        let read = self.source().filter(|read| read.id == id)?;
        Words::of(&read.tokens, from..read.next.checked_sub(short)?)
    }

    /// The words that made a word, its name just read from the source the
    /// word that made it was: from that word, which the reader read and
    /// ran, to the name. None known if the name came from another source.
    fn made_words(&self) -> Option<Source> {
        let (id, from) = self.invoked()?;
        let words = self.read_since(id, from, 0)?;
        Some(Source::of(Vec::new(), words))
    }

    /// Where the word read last is in a library's tokens: the source read
    /// first, and no other being read.
    fn position(&self) -> Option<usize> {
        let mut reads = self.frames.iter().filter_map(|frame| match frame {
            Frame::Read(read) => Some(read),
            Frame::Code { .. } => None,
        });
        match (self.frames.first(), reads.next(), reads.next()) {
            (Some(Frame::Read(_)), Some(read), None) => read.next.checked_sub(1),
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

    /// `create NAME`: a word that pushes where the data space goes on;
    /// `variable NAME`, one with a cell of its own there; `x constant
    /// NAME`, one that pushes x, copied in where it is compiled; `defer
    /// NAME`, one that runs the xt in a cell of its own (`is`).
    pub(crate) fn create(&mut self, word: Builtin, x: Option<Value<A>>) -> Result<(), Error> {
        let name = self.next_word(word, "a name")?;
        self.check_name(&name)?;
        let words = self.made_words();
        let body = self.next_address()?;
        if matches!(word, Builtin::Variable | Builtin::Defer) {
            self.allot(1)?;
        }
        match x {
            Some(x) => self.created(name.text, None, (false, words), vec![(Op::Push(x), None)]),
            None if word == Builtin::Defer => {
                let mut code = Steps::new(Rc::clone(&name.text));
                code.action = Some(body);
                code.push(Op::Push(Value::Addr(body)), None);
                code.push(Op::Builtin(Builtin::Fetch), None);
                code.push(Op::Builtin(Builtin::Execute), None);
                self.named_word(name.text, code, words);
            }
            None => self.created(name.text, Some(body), (false, words), Vec::new()),
        }
        Ok(())
    }

    /// `xt is NAME` and `action-of NAME`, `word`: the cell the action of
    /// NAME, a word `defer` made, is in, stored in or read (`op`), now or,
    /// compiling, as the code runs.
    pub(crate) fn action(&mut self, word: Builtin, op: Builtin) -> Result<(), Error> {
        let xt = (op == Builtin::Store)
            .then(|| self.value())
            .transpose()?
            .flatten();
        let name = self.next_word(word, "a word defer made")?;
        let cell = self.found(&name)?.xt.action.ok_or_else(|| {
            let why = format!("{} takes a word defer made, and this is none", word.name());
            Error::new(Code::BadName, why, Some(&name))
        })?;
        if self.compiling {
            self.compile(Op::Push(Value::Addr(cell)));
            self.compile(Op::Builtin(op));
            return Ok(());
        }
        if let Some(xt) = xt {
            self.cells(cell, 1)?.fill(Some(xt));
            return Ok(());
        }
        match self.cells(cell, 1)?.first().cloned().flatten() {
            Some(xt) => self.push(xt),
            None => {
                let why = format!("{} is given no xt yet: xt is {0}", name.text);
                return Err(self.fail(Code::Unset, why));
            }
        }
        Ok(())
    }

    /// `does` in a definition: a step that gives the word `create` made
    /// last what follows, which is compiled as code of its own, until the
    /// definition's `end` (`end_does`).
    pub(crate) fn does_part(&mut self) -> Result<(), Error> {
        if self.compiling_locals().is_none() {
            return Err(self.fail(Code::NotTopLevel, "does goes in a definition"));
        }
        let name = (self.building.last()).map_or_else(|| Rc::from(""), |b| Rc::clone(&b.code.name));
        let at = self.len();
        self.compile(Op::Does(0));
        let token = self.reading();
        self.begin(Kind::Does, name, None);
        self.mark(Construct::Does, at, token);
        Ok(())
    }

    /// The code after a `does`, closed by its definition's `end`: the
    /// code it is in steps to it.
    fn end_does(&mut self, mark: &Mark) {
        let Some(building) = self.building.pop() else {
            return;
        };
        self.compiling = building.was_compiling;
        let mut code = building.code;
        code.locals = building.locals.len();
        let action = self.version(code);
        self.patch(mark.at, action.index);
    }

    /// `does`, run in the code on top: the word `create` made last calls
    /// `action` after pushing its address, and the code returns. Out of
    /// the step loop's line: inlined there, it made every program slower
    /// (fuxth's bench, +1% instructions).
    #[cold]
    #[inline(never)]
    pub(crate) fn does(&mut self, action: usize) -> Result<(), Error> {
        let latest = (self.latest.as_ref()).and_then(|name| self.names.get_key_value(name));
        let made = latest.and_then(|(name, e)| {
            Some((
                Rc::clone(name),
                e.xt.body?,
                (e.xt.immediate, e.words.clone()),
            ))
        });
        let Some((name, body, made)) = made else {
            let why = "does gives the word create made last what it does, \
                       and the word defined last is none of create's";
            return Err(self.fail(Code::Type, why));
        };
        let slots = self.version_at(action).map_or(0, |code| code.locals);
        let call = vec![(Op::Call(action, slots), self.at())];
        self.created(name, Some(body), made, call);
        self.leave_definition(self.frames.len());
        Ok(())
    }

    /// `name` defined as the word defined last, immediate or not, made by
    /// `words`: it pushes `body`, if one, for `body-of`, then takes `steps`.
    fn created(
        &mut self,
        name: Rc<str>,
        body: Option<Address>,
        (immediate, words): (bool, Option<Source>),
        steps: Vec<(Op<A>, Option<Token>)>,
    ) {
        let mut code = Steps::new(Rc::clone(&name));
        code.body = body;
        code.immediate = immediate;
        let pushed = body.map(|body| (Op::Push(Value::Addr(body)), None));
        for (op, token) in pushed.into_iter().chain(steps) {
            code.push(op, token);
        }
        self.named_word(name, code, words);
    }

    /// `name` defined as `code`, made by `words`, the word defined last.
    fn named_word(&mut self, name: Rc<str>, code: Steps<A>, words: Option<Source>) {
        let xt = self.version(code);
        self.define_word(Rc::clone(&name), xt, words);
        self.latest = Some(name);
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
            Kind::Define(_) | Kind::Code | Kind::Does => Some(&mut building.locals),
            Kind::Temp => None,
        }
    }

    /// The value `let` or `to` sets, read before its name so too few
    /// values are said at the word: none while compiling.
    fn value(&mut self) -> Result<Option<Value<A>>, Error> {
        match self.compiling {
            true => Ok(None),
            false => self.pop().map(Some),
        }
    }

    /// `let NAME`.
    pub(crate) fn let_name(&mut self) -> Result<(), Error> {
        let value = self.value()?;
        let name = self.next_word(Builtin::Let, "a name")?;
        self.check_name(&name)?;
        if let Some(locals) = self.compiling_locals() {
            let next = locals.len();
            let slot = *locals.entry(Rc::clone(&name.text)).or_insert(next);
            self.compile(Op::SetLocal(slot));
            return Ok(());
        }
        let slot = match self.value_of(&name.text) {
            Some(slot) => slot,
            None => {
                let slot = self.globals.len();
                self.globals.push(None);
                let xt = self.one(&name.text, Op::GetGlobal(slot));
                let words = self.made_words();
                self.define_word(name.text, xt, words);
                slot
            }
        };
        self.set_global(slot, value);
        Ok(())
    }

    /// The top-level `let` name in `slot` set to `value`, or, compiling, a
    /// step that sets it.
    fn set_global(&mut self, slot: usize, value: Option<Value<A>>) {
        match value {
            Some(value) => {
                if let Some(global) = self.globals.get_mut(slot) {
                    *global = Some(value);
                }
            }
            None => self.compile(Op::SetGlobal(slot)),
        }
    }

    /// `to NAME`.
    pub(crate) fn set_name(&mut self) -> Result<(), Error> {
        let value = self.value()?;
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
        self.set_global(slot, value);
        Ok(())
    }

    /// `str WORD`, and `next-word`, which pushes it as it runs.
    pub(crate) fn string(&mut self, word: Builtin) -> Result<(), Error> {
        let text = self.next_word(word, "a word")?;
        let text = Value::Str(Rc::new(Text::new(&*text.text)));
        if word == Builtin::Str {
            self.given(text, true);
        } else {
            self.push(text);
        }
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
        match entry.xt.immediate {
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

    /// `immediate`: the word defined last made again, immediate, as a
    /// version of its own, so code compiled before keeps the one it was
    /// compiled against.
    pub(crate) fn immediate(&mut self) -> Result<(), Error> {
        let latest = (self.latest.clone())
            .and_then(|name| Some((Steps::clone(&self.names.get(&name)?.xt), name)));
        let Some((mut steps, name)) = latest else {
            return Err(self.fail(Code::BadName, "no word was defined yet"));
        };
        steps.immediate = true;
        let xt = self.version(steps);
        if let Some(entry) = self.names.get_mut(&name) {
            entry.xt = xt;
        }
        Ok(())
    }

    /// `see NAME`: what the dictionary says of NAME, written.
    pub(crate) fn see<H: Host<Value = A>>(&mut self, host: &mut H) -> Result<(), Error> {
        let name = self.next_word(Builtin::See, "a name")?;
        self.found(&name)?;
        let said = self.definition(&name.text).map(|d| d.to_string());
        self.written(host, &said.unwrap_or_default())
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

    /// `include PATH`, and `require PATH`: the file read next,
    /// through the host, relative to the file being read; required, a
    /// file read already is not read again, and one being read is a cycle.
    pub(crate) fn include<H: Host<Value = A>>(
        &mut self,
        host: &mut H,
        word: Builtin,
    ) -> Result<(), Error> {
        let once = word == Builtin::Require;
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
        let text = host.read(&canonical).map_err(|f| {
            Error::new(f.code, format!("{}: {}", path.text, f.message), at.as_ref())
        })?;
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
            | Value::Xt(_)
            | Value::Failure(_)
            | Value::Addr(_) => None,
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
            Some(format!("read as `str {text} {word}`"))
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
