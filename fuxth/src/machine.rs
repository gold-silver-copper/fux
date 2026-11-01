//! Running words. All a machine's state is data: the dictionary, the
//! stack, a frame for each call under way (the code and the step it is
//! at) and for each source being read (`reader`), the `let` names, the open
//! `list`, `each` and `try` blocks. So it can stop anywhere and go on
//! later: after a budget of steps, so its host can look for Ctrl-C, and at
//! an application word that needs its host to do something first. It never
//! runs a program as Rust recursion, so no program can overflow the Rust
//! stack, and stopping is just dropping it.
use crate::Code;
use crate::Failure;
use crate::error::{Error, Floor, Wants, underflow};
use crate::reader::{Building, Read};
use crate::token::Token;
use crate::value::{AppValue, List, Text, Thrown, Value, Xt};
use crate::vocabulary::Vocabulary;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// What runs the application's words.
pub trait Host {
    type Value: AppValue;
    /// What the host is asked to do while the machine waits.
    type Request;
    /// Runs application word `word` (its id in the vocabulary) on `args`,
    /// as many values as it takes, the deepest first. What it gives goes in
    /// `out`, and it says whether it is done, or the machine is to wait
    /// while the host does `Request` (then the host pushes its results, or
    /// raises its failure, and runs the machine again).
    fn call(
        &mut self,
        word: usize,
        args: Args<'_, Self::Value>,
        out: &mut Vec<Value<Self::Value>>,
    ) -> Result<Done<Self::Request>, Failure>;

    /// Writes text (`write`, `emit`, `print`), now.
    fn write(&mut self, text: &str) -> Result<(), Failure>;

    /// The file `path` names for `include` and `require`, as one identity
    /// (a canonical path): relative to `from`, the file being read, if one
    /// is. A host that reads no files refuses.
    fn resolve(&mut self, path: &str, from: Option<&Path>) -> Result<PathBuf, String> {
        let _ = (path, from);
        Err("this host reads no source files".to_owned())
    }

    /// The text of the file `resolve` gave.
    fn read(&mut self, path: &Path) -> Result<String, String> {
        let _ = path;
        Err("this host reads no source files".to_owned())
    }
}

/// Code: steps, each with the token it came from (none for a word of the
/// core's or the host's own), how many `let` names of its own it keeps,
/// the name it was defined with, and, a version of a machine's, where the
/// machine keeps it (`Machine::version`).
#[derive(Clone, Debug)]
pub(crate) struct Steps<A> {
    pub(crate) ops: Vec<Op<A>>,
    pub(crate) tokens: Vec<Option<Token>>,
    pub(crate) locals: usize,
    pub(crate) name: Rc<str>,
    pub(crate) index: usize,
}

impl<A> Steps<A> {
    pub(crate) fn new(name: Rc<str>) -> Self {
        Self {
            ops: Vec::new(),
            tokens: Vec::new(),
            locals: 0,
            name,
            index: 0,
        }
    }

    pub(crate) fn push(&mut self, op: Op<A>, token: Option<Token>) {
        self.ops.push(op);
        self.tokens.push(token);
    }

    pub(crate) fn len(&self) -> usize {
        self.ops.len()
    }

    /// Points the jump at `at` to `to`.
    pub(crate) fn patch(&mut self, at: usize, to: usize) {
        if let Some(
            Op::Jump(target)
            | Op::JumpUnless(target)
            | Op::EachNext(target)
            | Op::Tried(target)
            | Op::Try(target, _)
            | Op::Do(target),
        ) = self.ops.get_mut(at)
        {
            *target = to;
        }
    }

    fn token(&self, step: usize) -> Option<Token> {
        self.tokens.get(step).cloned().flatten()
    }

    /// Its one step, if it is one that runs where it is called and is
    /// copied in where it is compiled (`dup` is `0 pick`). Longer words
    /// stay calls, so a mistake in one is said as the whole word's, not one
    /// of its steps'.
    pub(crate) fn leaf(&self) -> Option<&Op<A>> {
        match self.ops.as_slice() {
            [
                op @ (Op::Push(_)
                | Op::Builtin(_)
                | Op::PickN(_)
                | Op::RollN(_)
                | Op::App(_)
                | Op::GetGlobal(_)),
            ] => Some(op),
            _ => None,
        }
    }
}

/// The values it holds are let go one after another, not as a recursion
/// (`value::release`), so code and lists nested however deep never
/// overflow the stack.
impl<A> Drop for Steps<A> {
    fn drop(&mut self) {
        let mut values = Vec::new();
        self.held(&mut values);
        crate::value::release(values);
    }
}

impl<A> Steps<A> {
    /// The values its steps hold, moved into `values`.
    pub(crate) fn held(&mut self, values: &mut Vec<Value<A>>) {
        for op in std::mem::take(&mut self.ops) {
            match op {
                Op::Push(value) => values.push(value),
                Op::Compile(code) => values.push(Value::Xt(Xt(code))),
                Op::Builtin(_)
                | Op::PickN(_)
                | Op::RollN(_)
                | Op::App(_)
                | Op::Call(..)
                | Op::Recurse
                | Op::Leave
                | Op::Jump(_)
                | Op::JumpUnless(_)
                | Op::SetLocal(_)
                | Op::GetLocal(_)
                | Op::SetGlobal(_)
                | Op::GetGlobal(_)
                | Op::ListStart
                | Op::Gather
                | Op::EachStart
                | Op::EachNext(_)
                | Op::Try(..)
                | Op::Tried(_)
                | Op::Do(_)
                | Op::Loop(_)
                | Op::LoopBy(_) => {}
            }
        }
    }
}

/// One step of compiled code.
#[derive(Clone, Debug)]
pub(crate) enum Op<A> {
    Push(Value<A>),
    Builtin(Builtin),
    /// `pick` with its count written just before it.
    PickN(usize),
    /// `roll` with its count written just before it.
    RollN(usize),
    App(usize),
    /// A definition, the version it was compiled against, by its index,
    /// and how many `let` names of its own it keeps.
    Call(usize, usize),
    /// The code the frame runs, called again (`recurse`).
    Recurse,
    /// Returns from the frame, leaving its loops (`leave-definition`).
    Leave,
    /// Compiles a call to the word into the code being compiled
    /// (`postpone` of a word that isn't immediate).
    Compile(Rc<Steps<A>>),
    Jump(usize),
    /// Takes a flag; jumps if it is false.
    JumpUnless(usize),
    /// A `let` name of the code's own, by its slot.
    SetLocal(usize),
    GetLocal(usize),
    /// A `let` name of the top level's (a value), by its slot.
    SetGlobal(usize),
    GetGlobal(usize),
    /// `list`: the stack from here is the list's.
    ListStart,
    /// `end` of a `list`: what is above where it began, gathered.
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
    /// `do`: takes the limit and the first index; a loop begun, or, with
    /// the two equal, a jump past it, to the step given.
    Do(usize),
    /// `loop`: the index one on; unless it reached the limit, back to the
    /// body's first step, given.
    Loop(usize),
    /// `loop-by`: takes how far the index moves, as `loop` moves it by 1.
    LoopBy(usize),
}

/// Whether the code returns from step `at` doing nothing more: it is past
/// the last step, or jumps there, through any number of jumps (the ends of
/// nested `if`s). A `try`'s body ends with a step of its own, so a call
/// last in it keeps the frame the `try` is in.
fn returns_from<A>(ops: &[Op<A>], mut at: usize) -> bool {
    // Each jump is followed once at most: a chain can't be longer.
    for _ in 0..=ops.len() {
        match ops.get(at) {
            None => return true,
            Some(Op::Jump(to)) => at = *to,
            Some(_) => return false,
        }
    }
    false
}

/// The values an application word takes, the deepest first, moved off the
/// stack as they are read. Those not read go too.
pub struct Args<'a, A> {
    stack: &'a mut Vec<Value<A>>,
    /// Where the word's values start.
    from: usize,
    at: usize,
}

impl<A> Iterator for Args<'_, A> {
    type Item = Value<A>;
    fn next(&mut self) -> Option<Value<A>> {
        let value = self.stack.get_mut(self.at)?;
        self.at = self.at.saturating_add(1);
        Some(std::mem::replace(value, Value::Bool(false)))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.stack.len().saturating_sub(self.at);
        (left, Some(left))
    }
}

impl<A> ExactSizeIterator for Args<'_, A> {}

impl<A> Drop for Args<'_, A> {
    fn drop(&mut self) {
        self.stack.truncate(self.from);
    }
}

/// An application word's answer: done, or to be waited for.
pub enum Done<R> {
    Now,
    Later(R),
}

/// How a run stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome<R> {
    /// Everything given was read and run.
    Finished,
    /// An application word waits for the host to do this.
    Suspended(R),
    /// The budget of steps ran out: run again to go on.
    Paused,
}

/// Something under way: a call, or a source being read.
#[derive(Clone, Debug)]
pub(crate) enum Frame {
    /// Code being run: its next step, where its `let` names start, and,
    /// for a call in last place, which took its caller's frame, the call
    /// that began the chain of such calls, as a code and the step after
    /// it. Only the first is kept, so a loop written as recursion keeps one.
    Code {
        code: usize,
        step: usize,
        via: Option<(usize, usize)>,
        locals: usize,
    },
    /// Boxed, so a call's frame, the one the step loop reads, is no
    /// larger for it.
    Read(Box<Read>),
}

/// An open block whose body starts with an empty stack: a `list`, or a
/// `try` with the codes it catches, its handler's step, and the frames,
/// `each` loops and code being compiled there were at it, to go back to.
/// Each holds how many frames there were at it.
#[derive(Clone, Debug)]
enum Body {
    List(usize),
    Try {
        codes: Rc<Vec<Code>>,
        handler: usize,
        frames: usize,
        iters: usize,
        loops: usize,
        returns: usize,
        building: usize,
        compiling: bool,
    },
}

/// An open `do` loop: its index and limit, the step after its end, and the
/// frames, `each` loops and open blocks there were at it, which `leave`
/// goes back to.
#[derive(Clone, Copy, Debug)]
struct Loop {
    index: i64,
    limit: i64,
    exit: usize,
    frames: usize,
    iters: usize,
    marks: usize,
}

impl Body {
    fn frames(&self) -> usize {
        match self {
            Body::List(frames) | Body::Try { frames, .. } => *frames,
        }
    }
}

/// A word in the dictionary: its code, and whether it runs even while a
/// definition is compiled.
#[derive(Clone, Debug)]
pub(crate) struct Entry<A> {
    pub(crate) xt: Rc<Steps<A>>,
    pub(crate) immediate: bool,
}

/// A machine: its dictionary and what it is running.
#[derive(Clone)]
pub struct Machine<A> {
    pub(crate) vocabulary: Rc<Vocabulary>,
    /// Every version of code the machine made, by index: frames and calls
    /// name code by it, so a call counts no references. Shared while a
    /// run reads it, and grown by `merge` from what was made since.
    versions: Rc<Vec<Rc<Steps<A>>>>,
    /// The versions made since the last `merge`, after `versions`.
    fresh: Vec<Rc<Steps<A>>>,
    /// Each name's word, the latest version: code compiled before keeps
    /// the version it was compiled against.
    pub(crate) names: HashMap<Rc<str>, Entry<A>>,
    /// The name defined last, which `immediate` marks.
    pub(crate) latest: Option<Rc<str>>,
    pub(crate) globals: Vec<Option<Value<A>>>,
    /// The code being compiled, the innermost last.
    pub(crate) building: Vec<Building<A>>,
    pub(crate) compiling: bool,
    /// How many codes were begun, which names each.
    pub(crate) begun: u64,
    /// The files read in: false while one is read, true once it was.
    pub(crate) loaded: HashMap<PathBuf, bool>,
    /// Where each definition of a library being read is in its tokens.
    pub(crate) made: Option<Vec<(usize, usize)>>,
    /// The word read last of a source read to its end: where everything
    /// read ends.
    pub(crate) read_last: Option<Token>,
    /// Whether it must end with an empty stack, as a callback.
    empty: bool,
    pub(crate) stack: Vec<Value<A>>,
    pub(crate) frames: Vec<Frame>,
    /// Every frame's `let` names, each frame's from its `locals`.
    locals: Vec<Option<Value<A>>>,
    /// Where each open `list` or `try` body began on the stack, and which:
    /// nothing below may be taken inside it.
    marks: Vec<(usize, Body)>,
    /// Each open `each`: what it walks, where its next element is (an
    /// index in a list, a byte in a string, so each step finds it at once),
    /// and how many frames there were at it.
    iters: Vec<(Sequence<A>, usize, usize)>,
    /// Each open `do` loop, the innermost last.
    loops: Vec<Loop>,
    /// The return stack (`to-return`): values only, never where a word
    /// returns to.
    returns: Vec<Value<A>>,
    /// The digits held so far between `digits-begin` and `digits-end`, the
    /// last first.
    digits: String,
    /// What application words give, kept between calls.
    out: Vec<Value<A>>,
    /// Steps taken in all, through every run.
    steps: u64,
    /// The one-character ASCII strings made so far, by code.
    chars: Vec<Option<Value<A>>>,
    /// The steps whose words took effect (`acted`), each once, in the order
    /// they first did: what a failure says had happened before it.
    actions: Vec<(Step, Token)>,
    /// How many sources were given to read, which names each.
    pub(crate) sources: u64,
    /// The calls under way where a failure `throw` raised again, and no
    /// `try` caught, first happened.
    raised: Vec<Token>,
    /// The step taken last: how many frames there were, its code, and its
    /// place in it.
    last: Option<(usize, usize, usize)>,
}

impl<A: AppValue> Machine<A> {
    /// A machine whose dictionary has the core's words and `vocabulary`'s.
    pub fn new(vocabulary: Vocabulary) -> Self {
        let words = vocabulary.names().count();
        let mut machine = Self {
            vocabulary: Rc::new(vocabulary),
            versions: Rc::new(Vec::new()),
            fresh: Vec::new(),
            names: HashMap::with_capacity(Builtin::ALL.len().saturating_add(words)),
            latest: None,
            globals: Vec::new(),
            building: Vec::new(),
            compiling: false,
            begun: 0,
            loaded: HashMap::new(),
            made: None,
            read_last: None,
            empty: false,
            stack: Vec::new(),
            frames: Vec::new(),
            locals: Vec::new(),
            marks: Vec::new(),
            iters: Vec::new(),
            loops: Vec::new(),
            returns: Vec::new(),
            digits: String::new(),
            out: Vec::new(),
            steps: 0,
            chars: Vec::new(),
            actions: Vec::new(),
            sources: 0,
            raised: Vec::new(),
            last: None,
        };
        let vocabulary = Rc::clone(&machine.vocabulary);
        for builtin in Builtin::ALL {
            if builtin.hosted() && !vocabulary.evaluates() {
                continue;
            }
            let xt = machine.one(builtin.name(), Op::Builtin(builtin));
            let entry = Entry {
                xt,
                immediate: builtin.immediate(),
            };
            machine.names.insert(Rc::from(builtin.name()), entry);
        }
        for (id, name) in vocabulary.names().enumerate() {
            let xt = machine.one(name, Op::App(id));
            let entry = Entry {
                xt,
                immediate: false,
            };
            machine.names.insert(Rc::from(name), entry);
        }
        machine.merge();
        machine
    }

    /// `steps` made a version of the machine's, where it is kept.
    pub(crate) fn version(&mut self, mut steps: Steps<A>) -> Rc<Steps<A>> {
        steps.index = self.versions.len().saturating_add(self.fresh.len());
        let steps = Rc::new(steps);
        self.fresh.push(Rc::clone(&steps));
        steps
    }

    /// A word that is one step, of the core's, the host's or a `let`.
    pub(crate) fn one(&mut self, name: &str, op: Op<A>) -> Rc<Steps<A>> {
        let mut steps = Steps::new(Rc::from(name));
        steps.push(op, None);
        self.version(steps)
    }

    /// The version at `index`.
    pub(crate) fn version_at(&self, index: usize) -> Option<&Rc<Steps<A>>> {
        match index.checked_sub(self.versions.len()) {
            None => self.versions.get(index),
            Some(fresh) => self.fresh.get(fresh),
        }
    }

    /// Where `xt` is kept here, if this machine made it (or the machine it
    /// was copied from, before the copy): its calls name versions by index,
    /// which mean other code in another machine.
    pub(crate) fn index_of(&self, xt: &Rc<Steps<A>>) -> Result<usize, Error> {
        match self.version_at(xt.index) {
            Some(code) if Rc::ptr_eq(code, xt) => Ok(xt.index),
            Some(_) | None => {
                let why = "an xt of another machine's: it runs only where it was made";
                Err(self.fail(Code::Type, why))
            }
        }
    }

    /// The versions made since, kept with the rest: copied first only if a
    /// run still reads the old ones.
    fn merge(&mut self) {
        if !self.fresh.is_empty() {
            Rc::make_mut(&mut self.versions).append(&mut self.fresh);
        }
    }

    /// The stack, deepest first.
    pub fn stack(&self) -> &[Value<A>] {
        &self.stack
    }

    /// Pushes a value.
    pub fn push(&mut self, value: Value<A>) {
        self.stack.push(value);
    }

    /// Stops what it runs and reads, and empties its stacks: its
    /// dictionary and its let names' values are kept.
    pub fn clear(&mut self) {
        while !self.frames.is_empty() {
            self.leave();
        }
        self.stack.clear();
        self.marks.clear();
        self.iters.clear();
        self.loops.clear();
        self.returns.clear();
        self.digits.clear();
        self.building.clear();
        self.compiling = false;
        self.last = None;
        self.actions.clear();
    }

    /// The word the machine is at took effect (the host did what it
    /// asked): listed with every failure from then on, at the innermost
    /// call under way whose token `skip` doesn't pass over (`calls`). Each
    /// step is listed once, however many times it acts; two steps are two,
    /// though one line holds both.
    pub fn acted(&mut self, skip: impl Fn(&Token) -> bool) {
        let found = self.steps().rev().find(|(_, token)| !skip(token));
        if let Some((step, token)) = found
            && !self.actions.iter().any(|(listed, _)| *listed == step)
        {
            self.actions.push((step, token));
        }
    }

    /// The steps whose words took effect, in the order they first did.
    pub fn actions(&self) -> impl Iterator<Item = &Token> {
        self.actions.iter().map(|(_, token)| token)
    }

    /// It is a callback's: what it is given must be used up, so it fails
    /// as it ends with values left (`unused`).
    pub fn callback(&mut self) {
        self.empty = true;
    }

    /// Raises a suspended word's failure at it, as if the word had failed
    /// when called: a `try` around it that catches its code goes on at its
    /// handler, when the machine runs again, its failure carrying `carried`
    /// if given (a failed wait's answer); else its error, said at the word,
    /// ends the run.
    pub fn raise(&mut self, failure: Failure, carried: Option<Value<A>>) -> Result<(), Error> {
        let error = self.failed(failure);
        self.recover(thrown(error, carried))
    }

    /// An application word's failure, said at the word, or the word it
    /// names.
    #[cold]
    pub(crate) fn failed(&self, failure: Failure) -> Error {
        let mut error = self.fail(failure.code, failure.message);
        error.word = failure.word.or(error.word);
        error
    }

    /// `thrown` caught by the innermost `try` that names its code: what the
    /// body began undone (its calls, sources, `let` frames, blocks, values
    /// and code being compiled), the failure pushed, with the actions taken
    /// so far, the handler next. Else its error, which no `try` caught, the
    /// machine left where it happened, for its host to say where, but for
    /// the code it was compiling.
    fn recover(&mut self, mut thrown: Thrown<A>) -> Result<(), Error> {
        let code = thrown.error.code;
        let catches = |(_, body): &(usize, Body)| matches!(body, Body::Try { codes, .. } if codes.contains(&code));
        let Some(caught) = self.marks.iter().rposition(catches) else {
            // Uncaught, the code being compiled means nothing: it and its
            // marks go, and the values stay.
            self.building.clear();
            self.compiling = false;
            self.stack.retain(|value| !matches!(value, Value::Mark(_)));
            if !thrown.calls.is_empty() {
                self.raised = thrown.calls;
            }
            return Err(thrown.error);
        };
        // Those inside it end with it.
        self.marks.truncate(caught.saturating_add(1));
        let Some((
            depth,
            Body::Try {
                handler,
                frames,
                iters,
                loops,
                returns,
                building,
                compiling,
                ..
            },
        )) = self.marks.pop()
        else {
            return Err(thrown.error);
        };
        // Where it happened, if calls end as it is caught (else they are
        // under way still); one raised again keeps where it first did.
        if thrown.calls.is_empty() && self.frames.len() > frames {
            thrown.calls = self.calls().collect();
        }
        while self.frames.len() > frames {
            self.leave();
        }
        self.stack.truncate(depth);
        self.iters.truncate(iters);
        self.loops.truncate(loops);
        self.returns.truncate(returns);
        self.building.truncate(building);
        self.compiling = compiling;
        self.jump(handler);
        let actions = self.actions().cloned().collect();
        self.push(Value::Failure(Rc::new(Thrown { actions, ..thrown })));
        Ok(())
    }

    /// `throw`: a failure raised again, or the failure of number `n` (none
    /// for 0).
    fn throw(&mut self, x: Value<A>) -> Result<(), Error> {
        let thrown = match x {
            Value::Int(0) => return Ok(()),
            Value::Int(n) => {
                let error = self.fail(Code::numbered(n), format!("{n} thrown"));
                Thrown {
                    number: Some(n),
                    ..thrown(error, None)
                }
            }
            Value::Failure(thrown) => Rc::unwrap_or_clone(thrown),
            other @ (Value::Bool(_)
            | Value::Str(_)
            | Value::List(_)
            | Value::App(_)
            | Value::Xt(_)
            | Value::Mark(_)) => {
                let app = matches!(other, Value::App(_));
                let wants = Wants::Takes(Builtin::Throw, "0, a number or a failure");
                return Err(self.refused(Some(wants), other.kind(), app));
            }
        };
        self.recover(thrown)
    }

    /// The step each call under way is at, the outermost first: the word
    /// read that began it, the word that called each definition, down to
    /// the word it ran last. A call in last place, which took its caller's
    /// frame, still shows the call that began it. Read from either end, it
    /// allocates nothing.
    pub fn calls(&self) -> impl DoubleEndedIterator<Item = Token> + '_ {
        self.steps().map(|(_, token)| token)
    }

    /// The calls under way where the failure that ended the run happened:
    /// for one `throw` raised again, those where it first did, which ended
    /// as a `try` caught it.
    pub fn failed_calls(&self) -> impl DoubleEndedIterator<Item = Token> + '_ {
        let live = self.raised.is_empty();
        (self.raised.iter().cloned()).chain(self.calls().filter(move |_| live))
    }

    /// `calls`, each with which step it is.
    fn steps(&self) -> impl DoubleEndedIterator<Item = (Step, Token)> + '_ {
        let (innermost, last) = (self.frames.len().saturating_sub(1), self.taken());
        self.frames.iter().enumerate().flat_map(move |(i, frame)| {
            let (via, at) = match frame {
                Frame::Read(read) => (None, read.step()),
                Frame::Code {
                    code, step, via, ..
                } => {
                    let via = via.and_then(|(code, step)| self.before(code, step));
                    let before = self.before(*code, *step);
                    // The innermost: the step it really took last, a jump's too.
                    let at = match i == innermost {
                        true => last.clone().or(before),
                        false => before,
                    };
                    (via, at)
                }
            };
            via.into_iter().chain(at)
        })
    }

    /// The step the innermost frame took last, and its token: none for a
    /// call just begun (its caller took the call).
    fn taken(&self) -> Option<(Step, Token)> {
        let (depth, code, step) = self.last?;
        let Some(Frame::Code { code: frame, .. }) = self.frames.last() else {
            return None;
        };
        let ours = depth == self.frames.len() && *frame == code;
        let token = ours.then(|| self.version_at(code)?.token(step))??;
        Some((Step::Code(code, step), token))
    }

    /// The step before `step` of the code at `code`, and its token: none at
    /// its first.
    fn before(&self, code: usize, step: usize) -> Option<(Step, Token)> {
        let step = step.checked_sub(1)?;
        Some((Step::Code(code, step), self.version_at(code)?.token(step)?))
    }

    /// The token the step just taken came from: where an error is, and
    /// the word a suspension is for, which the host's failure in doing it
    /// may name. In a frame at its first step (a call just begun, a loop
    /// just gone back), the call that began it, else the step about to be
    /// taken; the word read last, while reading. A word of the core's or
    /// the host's run by `execute` has no token of its own: its caller's.
    pub fn at(&self) -> Option<Token> {
        if let Some((_, taken)) = self.taken() {
            return Some(taken);
        }
        self.frames.iter().rev().find_map(|frame| match frame {
            Frame::Read(read) => read.last(),
            Frame::Code {
                code, step, via, ..
            } => (self.before(*code, *step))
                .or_else(|| via.and_then(|(code, step)| self.before(code, step)))
                .map(|(_, token)| token)
                .or_else(|| self.version_at(*code)?.token(*step)),
        })
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn fail(&self, code: Code, message: impl Into<String>) -> Error {
        Error::new(code, message, self.at().as_ref())
    }

    /// Ends the frame on top: its `let` names go with it; a file whose
    /// reading ends so, unfinished, was never read.
    pub(crate) fn leave(&mut self) {
        match self.frames.pop() {
            Some(Frame::Code { locals, .. }) => self.locals.truncate(locals),
            Some(Frame::Read(read)) => {
                if let Some(path) = read.path {
                    self.loaded.remove(&path);
                }
            }
            None => {}
        }
    }

    /// Runs `code`, one of this machine's, in a frame of its own.
    pub(crate) fn enter(&mut self, code: &Rc<Steps<A>>) -> Result<(), Error> {
        let index = self.index_of(code)?;
        let base = self.locals.len();
        self.locals.resize(base.saturating_add(code.locals), None);
        self.frames.push(Frame::Code {
            code: index,
            step: 0,
            via: None,
            locals: base,
        });
        Ok(())
    }

    /// Runs up to `budget` steps: each word read is one, as each step of
    /// code is.
    pub fn run<H: Host<Value = A>>(
        &mut self,
        host: &mut H,
        budget: u64,
    ) -> Result<Outcome<H::Request>, Error> {
        let until = self.steps.saturating_add(budget);
        self.raised.clear();
        loop {
            match self.steps_until(host, until) {
                Err(error) => self.recover(thrown(error, None))?,
                ran => return ran,
            }
        }
    }

    /// Runs until `until` steps are taken in all, or a step fails.
    #[inline(never)]
    fn steps_until<H: Host<Value = A>>(
        &mut self,
        host: &mut H,
        until: u64,
    ) -> Result<Outcome<H::Request>, Error> {
        // The versions as the run began: a call names its code by index,
        // and the run reads it here, counting no references; code made
        // since is merged in as the run first enters it.
        let mut versions = Some(Rc::clone(&self.versions));
        'frames: loop {
            let depth = self.frames.len();
            let index = match self.frames.last() {
                None => {
                    self.ended()?;
                    return Ok(Outcome::Finished);
                }
                Some(Frame::Read(_)) => {
                    if self.steps >= until {
                        return Ok(Outcome::Paused);
                    }
                    self.steps = self.steps.saturating_add(1);
                    if let Some(request) = self.read_word(host)? {
                        return Ok(Outcome::Suspended(request));
                    }
                    continue 'frames;
                }
                Some(Frame::Code { code, .. }) => *code,
            };
            if versions.as_ref().is_none_or(|v| index >= v.len()) {
                // Let go first, so the merge copies nothing.
                drop(versions.take());
                self.merge();
                versions = Some(Rc::clone(&self.versions));
            }
            let Some(code) = versions.as_ref().and_then(|v| v.get(index)) else {
                self.leave();
                continue 'frames;
            };
            loop {
                if self.steps >= until {
                    return Ok(Outcome::Paused);
                }
                self.steps = self.steps.saturating_add(1);
                let Some(Frame::Code { step, .. }) = self.frames.last_mut() else {
                    continue 'frames;
                };
                // Past its last step, code returns: its `let` names go.
                let Some(op) = code.ops.get(*step) else {
                    if let Some(Frame::Code { locals, .. }) = self.frames.pop() {
                        self.locals.truncate(locals);
                    }
                    continue 'frames;
                };
                self.last = Some((depth, index, *step));
                *step = step.saturating_add(1);
                let after = *step;
                match op {
                    // Cloned straight into the stack: through `push`, the
                    // value is written to a temporary in parts and read back
                    // whole, which the CPU can't forward (counting to a
                    // million: +7% cycles on fewer instructions).
                    Op::Push(value) => self.stack.extend_from_slice(std::slice::from_ref(value)),
                    Op::Builtin(builtin) => {
                        self.builtin(*builtin, host)?;
                        if self.frames.len() != depth {
                            continue 'frames;
                        }
                    }
                    Op::PickN(n) => self.pick(*n)?,
                    Op::RollN(n) => self.roll(*n)?,
                    Op::App(id) => {
                        if let Some(request) = self.app(*id, host)? {
                            return Ok(Outcome::Suspended(request));
                        }
                    }
                    Op::Call(..) | Op::Recurse => {
                        // The code called, or the frame's own (`recurse`).
                        let called = if let Op::Call(called, slots) = op {
                            (*called, *slots)
                        } else {
                            (index, code.locals)
                        };
                        self.call(called, code, index, after);
                        continue 'frames;
                    }
                    Op::Leave => {
                        self.iters.retain(|(.., frames)| *frames < depth);
                        self.loops.retain(|l| l.frames < depth);
                        while self.marks.last().is_some_and(|(_, b)| b.frames() >= depth) {
                            self.marks.pop();
                        }
                        self.leave();
                        continue 'frames;
                    }
                    Op::Compile(xt) => self.compile_word(xt)?,
                    Op::Jump(to) => *step = *to,
                    Op::JumpUnless(to) => {
                        if !self.flag()? {
                            self.jump(*to);
                        }
                    }
                    Op::SetLocal(slot) => {
                        let value = self.pop()?;
                        if let Some(local) = self.local(*slot) {
                            *local = Some(value);
                        }
                    }
                    Op::GetLocal(slot) => {
                        let value = self.local(*slot).and_then(|v| v.clone());
                        self.named(value)?;
                    }
                    Op::SetGlobal(slot) => {
                        let value = self.pop()?;
                        if let Some(global) = self.globals.get_mut(*slot) {
                            *global = Some(value);
                        }
                    }
                    Op::GetGlobal(slot) => self.global(*slot)?,
                    Op::ListStart => self.marks.push((self.stack.len(), Body::List(depth))),
                    Op::Gather => {
                        let from = (self.marks.pop())
                            .map_or(0, |(at, _)| at)
                            .min(self.stack.len());
                        let mut items = Vec::with_capacity(self.stack.len().saturating_sub(from));
                        while self.stack.len() > from {
                            items.extend(self.stack.pop());
                        }
                        items.reverse();
                        let list = Self::list(items);
                        self.push(list);
                    }
                    Op::EachStart => {
                        let walked = self.pop()?;
                        let app = matches!(walked, Value::App(_));
                        match Seq::of(walked) {
                            Ok(seq) => self.iters.push((seq, 0, depth)),
                            Err(kind) => {
                                return Err(self.fail(Code::Type, Wants::Each.not(kind, app)));
                            }
                        }
                    }
                    Op::EachNext(out) => {
                        let next = (self.iters.last_mut()).and_then(|(seq, at, _)| next(seq, at));
                        match next {
                            Some(item) => self.push(item),
                            None => {
                                self.iters.pop();
                                self.jump(*out);
                            }
                        }
                    }
                    Op::Try(handler, codes) => {
                        let body = Body::Try {
                            codes: Rc::clone(codes),
                            handler: *handler,
                            frames: depth,
                            iters: self.iters.len(),
                            loops: self.loops.len(),
                            returns: self.returns.len(),
                            building: self.building.len(),
                            compiling: self.compiling,
                        };
                        self.marks.push((self.stack.len(), body));
                    }
                    Op::Tried(out) => {
                        self.marks.pop();
                        *step = *out;
                    }
                    Op::Do(exit) => self.start_loop(*exit, depth)?,
                    Op::Loop(body) => self.next_loop(1, *body),
                    Op::LoopBy(body) => {
                        let by = self.pop()?;
                        let by = self.int(Builtin::LoopBy, by)?;
                        self.next_loop(by, *body);
                    }
                }
            }
        }
    }

    /// Calls `called` from `code`, whose next step is `step`. A call that
    /// is its caller's last step replaces it: the caller has nothing left
    /// to do, so a loop written as recursion runs in one frame.
    fn call(
        &mut self,
        (called, slots): (usize, usize),
        code: &Steps<A>,
        index: usize,
        step: usize,
    ) {
        let mut via = None;
        if returns_from(&code.ops, step)
            && let Some(Frame::Code {
                via: was, locals, ..
            }) = self.frames.pop()
        {
            via = was.or(Some((index, step)));
            self.locals.truncate(locals);
        }
        let base = self.locals.len();
        if slots > 0 {
            self.locals.resize(base.saturating_add(slots), None);
        }
        self.frames.push(Frame::Code {
            code: called,
            step: 0,
            via,
            locals: base,
        });
    }

    /// `do`: the limit and the first index taken, and a loop begun at the
    /// step after, in the frame `frames` deep; with the two equal, none,
    /// and on at `exit`, past its end.
    #[inline(never)]
    fn start_loop(&mut self, exit: usize, frames: usize) -> Result<(), Error> {
        self.depth(2)?;
        let [limit, index] = self.moved();
        let (limit, index) = (self.int(Builtin::Do, limit)?, self.int(Builtin::Do, index)?);
        if index == limit {
            self.jump(exit);
            return Ok(());
        }
        let (iters, marks) = (self.iters.len(), self.marks.len());
        self.loops.push(Loop {
            index,
            limit,
            exit,
            frames,
            iters,
            marks,
        });
        Ok(())
    }

    /// `loop` and `loop-by`: the innermost loop's index moved `by`, and
    /// back to `body`, unless it crossed from just under the limit to the
    /// limit, or, moving down, from the limit to just under it, as Forth's
    /// `+loop` does: then the loop ends.
    #[inline(never)]
    fn next_loop(&mut self, by: i64, body: usize) {
        let Some(running) = self.loops.last_mut() else {
            return;
        };
        let was = running.index.wrapping_sub(running.limit);
        let now = was.wrapping_add(by);
        running.index = running.index.wrapping_add(by);
        if (was ^ now) < 0 && (now ^ by) >= 0 {
            self.loops.pop();
        } else {
            self.jump(body);
        }
    }

    /// The `nth` innermost `do` loop of the code running (0 for the
    /// innermost), for `word`, which goes in that many loops and one.
    fn running_loop(&self, nth: usize, word: Builtin) -> Result<Loop, Error> {
        let depth = self.running_depth();
        let mut ours = self.loops.iter().rev().take_while(|l| l.frames == depth);
        ours.nth(nth).copied().ok_or_else(|| {
            let what = if nth == 0 {
                "a do … loop"
            } else {
                "a do … loop in another"
            };
            self.fail(Code::NotTopLevel, format!("{} goes in {what}", word.name()))
        })
    }

    /// How many frames deep the code running is: a core word's xt run by
    /// `execute` runs in a frame of its own, over its caller's, and acts
    /// in the caller's, as compiled there it would.
    fn running_depth(&self) -> usize {
        let dispatched = |frame: &Frame| match frame {
            Frame::Code { code, .. } => {
                let leaf = self.version_at(*code).and_then(|code| code.leaf());
                matches!(leaf, Some(Op::Builtin(_)))
            }
            Frame::Read(_) => false,
        };
        let above = self
            .frames
            .iter()
            .rev()
            .take_while(|f| dispatched(f))
            .count();
        self.frames.len().saturating_sub(above)
    }

    /// `leave`: the innermost loop ended, and the `each` loops and blocks
    /// begun in it, and on past its end, in the frame that runs it.
    fn leave_loop(&mut self) -> Result<(), Error> {
        let running = self.running_loop(0, Builtin::Leave)?;
        while self.frames.len() > running.frames {
            self.leave();
        }
        self.loops.pop();
        self.iters.truncate(running.iters);
        self.marks.truncate(running.marks);
        self.jump(running.exit);
        Ok(())
    }

    /// Refused unless the return stack has a value: in a `try`'s body,
    /// one put there in the body, as a caught failure keeps those below.
    fn returned(&self) -> Result<(), Error> {
        let floor = self.marks.iter().rev().find_map(|(_, body)| match body {
            Body::Try { returns, .. } => Some(*returns),
            Body::List(_) => None,
        });
        if self.returns.len() > floor.unwrap_or(0) {
            return Ok(());
        }
        let why = match floor {
            Some(_) => "the return stack has nothing put there inside this try … catch",
            None => "the return stack is empty",
        };
        Err(self.fail(Code::StackUnderflow, why))
    }

    /// The innermost code frame's `let` name in `slot`.
    fn local(&mut self, slot: usize) -> Option<&mut Option<Value<A>>> {
        let Some(Frame::Code { locals, .. }) = self.frames.last() else {
            return None;
        };
        self.locals.get_mut(locals.saturating_add(slot))
    }

    /// A top-level `let` name's value pushed.
    pub(crate) fn global(&mut self, slot: usize) -> Result<(), Error> {
        let value = self.globals.get(slot).cloned().flatten();
        self.named(value)
    }

    /// Runs `xt`, a word read: a value or an application word at once (the
    /// latter giving what it waits for, if it does), any other in a frame
    /// of its own. Only the step loop runs a word of the core's, so its
    /// dispatch is inlined there.
    pub(crate) fn invoke<H: Host<Value = A>>(
        &mut self,
        xt: Rc<Steps<A>>,
        host: &mut H,
    ) -> Result<Option<H::Request>, Error> {
        match xt.leaf() {
            Some(Op::App(id)) => self.app(*id, host),
            Some(Op::Push(value)) => {
                self.push(value.clone());
                Ok(None)
            }
            _ => self.enter(&xt).map(|()| None),
        }
    }

    /// Application word `id` run on the stack's top values: the request
    /// to wait for, if it suspends. Kept out of `run`'s loop, with the
    /// failures below: inlined there, the host's code made every step of a
    /// program that calls none slower (fux's pure loop, +1.2% instructions).
    #[inline(never)]
    pub(crate) fn app<H: Host<Value = A>>(
        &mut self,
        id: usize,
        host: &mut H,
    ) -> Result<Option<H::Request>, Error> {
        let from = self.depth(self.vocabulary.takes(id))?;
        let mut out = std::mem::take(&mut self.out);
        out.clear();
        let args = Args {
            stack: &mut self.stack,
            from,
            at: from,
        };
        let done = host.call(id, args, &mut out).map_err(|f| self.failed(f))?;
        self.stack.append(&mut out);
        self.out = out;
        match done {
            Done::Now => Ok(None),
            // What it gives comes with the host's answer.
            Done::Later(request) => Ok(Some(request)),
        }
    }

    /// A `let` name's value pushed; `unset` if its `let` hasn't run.
    fn named(&mut self, value: Option<Value<A>>) -> Result<(), Error> {
        match value {
            Some(value) => {
                self.push(value);
                Ok(())
            }
            None => Err(self.fail(Code::Unset, "read before its let ran")),
        }
    }

    /// Everything read: code left open, or a value left that must be
    /// used, fails it.
    fn ended(&self) -> Result<(), Error> {
        if !self.building.is_empty() {
            return Err(self.unclosed());
        }
        let unused = |why| Error::new(Code::Unused, why, self.read_last.as_ref());
        if self.empty && !self.stack.is_empty() {
            return Err(unused("a callback must leave an empty stack"));
        }
        let words = &self.vocabulary;
        if let Some(why) = self.stack.iter().find_map(|v| words.unused(v)) {
            return Err(unused(why));
        }
        Ok(())
    }

    fn jump(&mut self, to: usize) {
        if let Some(Frame::Code { step, .. }) = self.frames.last_mut() {
            *step = to;
        }
    }

    /// Where the top `n` values start, if the stack has them: inside a
    /// `list`, only those above where it began.
    #[inline(always)]
    fn depth(&self, n: usize) -> Result<usize, Error> {
        let floor = self.marks.last().map_or(0, |(at, _)| *at);
        let has = self.stack.len().saturating_sub(floor);
        if has >= n {
            return Ok(self.stack.len().saturating_sub(n));
        }
        Err(self.short(n, has))
    }

    /// Too few values: `n` taken, and `has` there. Out of line, so the
    /// words that take values are inlined where they run.
    #[cold]
    #[inline(never)]
    fn short(&self, n: usize, has: usize) -> Error {
        let inside = self.marks.last().map(|(_, body)| match body {
            Body::List(_) => Floor::List,
            Body::Try { .. } => Floor::Try,
        });
        self.fail(Code::StackUnderflow, underflow(n, has, inside))
    }

    pub(crate) fn pop(&mut self) -> Result<Value<A>, Error> {
        self.depth(1)?;
        let [value] = self.moved();
        Ok(value)
    }

    /// Takes a flag, as `if` and `until` do: a boolean, or an integer,
    /// true unless it is 0.
    fn flag(&mut self) -> Result<bool, Error> {
        match self.pop()? {
            Value::Bool(flag) => Ok(flag),
            Value::Int(n) => Ok(n != 0),
            other @ (Value::Str(_)
            | Value::List(_)
            | Value::App(_)
            | Value::Xt(_)
            | Value::Mark(_)
            | Value::Failure(_)) => {
                Err(self.fail(Code::Type, Wants::Flag.not(other.kind(), false)))
            }
        }
    }

    /// `value` as an integer, for `word`.
    fn int(&self, word: Builtin, value: Value<A>) -> Result<i64, Error> {
        let app = matches!(value, Value::App(_));
        Int::of(value).map_err(|got| self.refused(Int::wants(word), got, app))
    }

    /// The character with code `n`.
    fn character(&self, n: i64) -> Result<char, Error> {
        let c = u32::try_from(n).ok().and_then(char::from_u32);
        c.ok_or_else(|| self.fail(Code::Range, format!("{n} is no character code")))
    }

    /// `word` on two booleans, `logical`, or on two integers, `bits`.
    fn logic(
        &mut self,
        word: Builtin,
        (a, b): (Value<A>, Value<A>),
        logical: fn(bool, bool) -> bool,
        bits: fn(i64, i64) -> i64,
    ) -> Result<(), Error> {
        let value = match (a, b) {
            (Value::Bool(a), Value::Bool(b)) => Value::Bool(logical(a, b)),
            (Value::Int(a), Value::Int(b)) => Value::Int(bits(a, b)),
            (a, b) => {
                let app = matches!(a, Value::App(_)) || matches!(b, Value::App(_));
                let got = format!("{} and {}", a.kind(), b.kind());
                let wants = Wants::Takes(word, "two booleans or two integers");
                return Err(self.refused(Some(wants), &got, app));
            }
        };
        self.push(value);
        Ok(())
    }

    /// `n` as `word`'s count: 0 or more.
    fn count(&self, word: Builtin, n: i64) -> Result<usize, Error> {
        usize::try_from(n).map_err(|_| {
            self.fail(
                Code::Range,
                format!("{} takes a count from 0, not {n}", word.name()),
            )
        })
    }

    /// `n` as an index into something `len` long.
    fn index(&self, n: i64, len: usize) -> Result<usize, Error> {
        (usize::try_from(n).ok())
            .filter(|i| *i < len)
            .ok_or_else(|| self.fail(Code::Range, format!("index {n}, and it has {len}")))
    }

    /// `got` given in a place that `wants` says what it takes.
    #[cold]
    fn refused(&self, wants: Option<Wants>, got: &str, app: bool) -> Error {
        self.fail(
            Code::Type,
            wants.map_or_else(|| got.to_owned(), |wants| wants.not(got, app)),
        )
    }

    /// A built-in's `N` values off the stack, the deepest first, once
    /// `depth` found the stack has them (too few are said as the word
    /// takes them, before any is taken): apart from that check, so the
    /// values never go through its failure's result.
    #[inline]
    fn moved<const N: usize>(&mut self) -> [Value<A>; N] {
        // None is the `false` put for one it lacks.
        let mut taken =
            std::array::from_fn(|_| self.stack.pop().unwrap_or_else(|| Value::Bool(false)));
        taken.reverse();
        taken
    }

    /// `text` as a new string.
    pub(crate) fn text(text: &str) -> Value<A> {
        Value::str(text)
    }

    /// The string of `c`, an ASCII one made once a run.
    fn char(&mut self, c: char) -> Value<A> {
        let code = usize::from(u8::try_from(c).unwrap_or(u8::MAX));
        if let Some(Some(made)) = self.chars.get(code) {
            return made.clone();
        }
        let made = Self::text(c.encode_utf8(&mut [0; 4]));
        if c.is_ascii() {
            self.chars.resize(128, None);
        }
        if let Some(slot) = self.chars.get_mut(code) {
            *slot = Some(made.clone());
        }
        made
    }

    /// A list of `items`.
    fn list(items: Vec<Value<A>>) -> Value<A> {
        Value::List(Rc::new(List::new(items)))
    }

    fn pick(&mut self, n: usize) -> Result<(), Error> {
        let from = self.depth(n.saturating_add(1))?;
        match self.stack.get(from).cloned() {
            Some(value) => {
                self.push(value);
                Ok(())
            }
            None => Err(self.fail(Code::StackUnderflow, "pick reached past the stack")),
        }
    }

    fn roll(&mut self, n: usize) -> Result<(), Error> {
        let from = self.depth(n.saturating_add(1))?;
        if let Some(values) = self.stack.get_mut(from..) {
            values.rotate_left(1);
        }
        Ok(())
    }

    /// `n` divided by `m`, wrapping as the rest of arithmetic does; by 0,
    /// refused.
    fn divided(&mut self, n: i64, m: i64) -> Result<(), Error> {
        if m == 0 {
            return Err(self.fail(Code::DivByZero, "division by zero"));
        }
        // Only MIN div -1 is past 64 bits, and wraps around to MIN.
        self.push(Value::Int(n.checked_div(m).unwrap_or(n)));
        Ok(())
    }

    fn concat(&self, list: &List<A>) -> Result<Value<A>, Error> {
        let (mut strings, mut lists) = (false, false);
        for item in list.items() {
            match item {
                Value::Str(_) => strings = true,
                Value::List(_) => lists = true,
                Value::Int(_)
                | Value::Bool(_)
                | Value::App(_)
                | Value::Xt(_)
                | Value::Mark(_)
                | Value::Failure(_) => {
                    return Err(self.fail(
                        Code::Type,
                        format!(
                            "concat takes a list of strings or of lists, and this one holds {}",
                            item.kind()
                        ),
                    ));
                }
            }
            if strings && lists {
                return Err(self.fail(
                    Code::Type,
                    "concat takes a list of strings or of lists, not both",
                ));
            }
        }
        if strings {
            let strings = list.items().iter().filter_map(|item| match item {
                Value::Str(s) => Some::<&str>(s),
                Value::Int(_)
                | Value::Bool(_)
                | Value::List(_)
                | Value::App(_)
                | Value::Xt(_)
                | Value::Mark(_)
                | Value::Failure(_) => None,
            });
            return Ok(Self::text(&strings.collect::<String>()));
        }
        let mut items = Vec::new();
        for item in list.items() {
            if let Value::List(l) = item {
                items.extend_from_slice(l.items());
            }
        }
        Ok(Self::list(items))
    }

    /// `x` added at the end, in place: copied first when something else
    /// holds it.
    fn pushed(&self, seq: Sequence<A>, x: Value<A>) -> Result<Value<A>, Error> {
        match (seq, x) {
            (Sequence::List(mut list), x) => {
                Rc::make_mut(&mut list).push(x);
                Ok(Value::List(list))
            }
            (Sequence::Str(mut s), Value::Str(x)) => {
                Rc::make_mut(&mut s).append(&x);
                Ok(Value::Str(s))
            }
            (Sequence::Str(_), x) => {
                let app = matches!(x, Value::App(_));
                Err(self.fail(Code::Type, Wants::PushText.not(x.kind(), app)))
            }
        }
    }

    fn put(&self, seq: Sequence<A>, i: usize, x: Value<A>) -> Result<Value<A>, Error> {
        match (seq, x) {
            (Sequence::List(mut list), x) => {
                let put = Rc::make_mut(&mut list).put(i, x);
                put.ok_or_else(|| self.fail(Code::Range, format!("index {i}, past the end")))?;
                Ok(Value::List(list))
            }
            (Sequence::Str(s), Value::Str(x)) => {
                // One character replaced by x's: the string is made anew.
                let (head, tail) = (s.part(0, i), s.part(i.saturating_add(1), s.chars_len()));
                Ok(Self::text(&[head, &**x, tail].concat()))
            }
            (Sequence::Str(_), x) => {
                let app = matches!(x, Value::App(_));
                Err(self.fail(Code::Type, Wants::PutText.not(x.kind(), app)))
            }
        }
    }

    /// Writes `text` through the host, said at the word.
    fn written<H: Host<Value = A>>(&self, host: &mut H, text: &str) -> Result<(), Error> {
        host.write(text).map_err(|f| self.fail(f.code, f.message))
    }
}

/// Which step of which code, or which word of which source read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Code(usize, usize),
    Read(u64, usize),
}

impl Read {
    /// The word it read last, and which it is.
    fn step(&self) -> Option<(Step, Token)> {
        let at = self.next.checked_sub(1)?;
        Some((Step::Read(self.id, at), self.last()?))
    }
}

/// `error` as a failure to raise, carrying `carried`: its number its code's.
fn thrown<A>(error: Error, carried: Option<Value<A>>) -> Thrown<A> {
    Thrown {
        number: error.code.number(),
        error,
        carried,
        actions: Vec::new(),
        calls: Vec::new(),
    }
}

/// The element of `seq` at `at` (a byte in a string, an index in a list),
/// and `at` moved past it: a string's character as its code.
fn next<A: Clone>(seq: &Sequence<A>, at: &mut usize) -> Option<Value<A>> {
    match seq {
        Sequence::Str(s) => {
            let c = s.get(*at..).and_then(|rest| rest.chars().next());
            *at = at.saturating_add(c.map_or(1, char::len_utf8));
            c.map(|c| Value::Int(i64::from(u32::from(c))))
        }
        Sequence::List(list) => {
            let item = list.items().get(*at).cloned();
            *at = at.saturating_add(1);
            item
        }
    }
}

/// Why `lt` can't order two values: their kinds, or, in two lists, the
/// first elements that can't be ordered, found through nested lists.
fn unordered<A: AppValue>(a: &Value<A>, b: &Value<A>) -> String {
    let mut path = Vec::new();
    let (mut a, mut b) = (a, b);
    while let (Value::List(x), Value::List(y)) = (a, b) {
        let next = (x.items().iter().zip(y.items()).enumerate())
            .find(|(_, (p, q))| p != q && compare(p, q).is_none());
        let Some((i, (p, q))) = next else {
            break;
        };
        path.push(format!("element {i}"));
        (a, b) = (p, q);
    }
    if path.is_empty() {
        let app = matches!(a, Value::App(_)) || matches!(b, Value::App(_));
        Wants::Ordered.not(format_args!("{} and {}", a.kind(), b.kind()), app)
    } else {
        format!(
            "lt compares lists element by element, and at {} it finds {} and {}",
            path.join(", "),
            a.kind(),
            b.kind()
        )
    }
}

/// How `lt` orders two values: integers by value, strings by their
/// characters' codes, lists element by element (a shorter prefix first);
/// `None` for any other two. Lists are walked down to their first unequal
/// elements, not as a recursion, so lists nested however deep compare.
fn compare<A: PartialEq>(mut a: &Value<A>, mut b: &Value<A>) -> Option<Ordering> {
    loop {
        match (a, b) {
            (Value::Int(a), Value::Int(b)) => return Some(a.cmp(b)),
            (Value::Str(a), Value::Str(b)) => return Some(a.as_ref().cmp(b.as_ref())),
            (Value::List(x), Value::List(y)) => {
                let unequal = x.items().iter().zip(y.items()).find(|(p, q)| p != q);
                let Some((p, q)) = unequal else {
                    return Some(x.items().len().cmp(&y.items().len()));
                };
                (a, b) = (p, q);
            }
            (
                Value::Int(_)
                | Value::Bool(_)
                | Value::Str(_)
                | Value::List(_)
                | Value::App(_)
                | Value::Xt(_)
                | Value::Mark(_)
                | Value::Failure(_),
                _,
            ) => {
                return None;
            }
        }
    }
}

/// A string or a list: what the sequence words take, and `each` walks.
#[derive(Clone, Debug)]
pub(crate) enum Sequence<A> {
    Str(Rc<Text>),
    List(Rc<List<A>>),
}

impl<A: AppValue> Sequence<A> {
    /// How many elements it has.
    fn len(&self) -> usize {
        match self {
            Self::Str(s) => s.chars_len(),
            Self::List(list) => list.items().len(),
        }
    }
}

/// The kinds of value a built-in's place takes, each a marker: `of`, which
/// turns a value into what the word's body takes, or says what it is
/// instead, and `wants`, what a value of another kind is told (none for a
/// place that takes any value).
struct Any;

impl Any {
    fn wants(_: Builtin) -> Option<Wants> {
        None
    }

    fn of<A>(value: Value<A>) -> Result<Value<A>, &'static str> {
        Ok(value)
    }
}

/// An integer.
struct Int;

impl Int {
    fn wants(word: Builtin) -> Option<Wants> {
        Some(Wants::Int(word))
    }

    fn of<A: AppValue>(value: Value<A>) -> Result<i64, &'static str> {
        match value {
            Value::Int(n) => Ok(n),
            other @ (Value::Bool(_)
            | Value::Str(_)
            | Value::List(_)
            | Value::App(_)
            | Value::Xt(_)
            | Value::Mark(_)
            | Value::Failure(_)) => Err(other.kind()),
        }
    }
}

/// A string.
struct Str;

impl Str {
    fn wants(word: Builtin) -> Option<Wants> {
        Some(Wants::Takes(word, "a string"))
    }

    fn of<A: AppValue>(value: Value<A>) -> Result<Rc<Text>, &'static str> {
        match value {
            Value::Str(s) => Ok(s),
            other @ (Value::Int(_)
            | Value::Bool(_)
            | Value::List(_)
            | Value::App(_)
            | Value::Xt(_)
            | Value::Mark(_)
            | Value::Failure(_)) => Err(other.kind()),
        }
    }
}

/// An execution token, as the code it runs.
struct Exec;

impl Exec {
    fn wants(word: Builtin) -> Option<Wants> {
        Some(Wants::Takes(word, "an xt"))
    }

    fn of<A: AppValue>(value: Value<A>) -> Result<Rc<Steps<A>>, &'static str> {
        match value {
            Value::Xt(Xt(code)) => Ok(code),
            other @ (Value::Int(_)
            | Value::Bool(_)
            | Value::Str(_)
            | Value::List(_)
            | Value::App(_)
            | Value::Mark(_)
            | Value::Failure(_)) => Err(other.kind()),
        }
    }
}

/// A string or a list.
struct Seq;

impl Seq {
    fn wants(word: Builtin) -> Option<Wants> {
        Some(Wants::Seq(word))
    }

    fn of<A: AppValue>(value: Value<A>) -> Result<Sequence<A>, &'static str> {
        match value {
            Value::Str(s) => Ok(Sequence::Str(s)),
            Value::List(list) => Ok(Sequence::List(list)),
            other @ (Value::Int(_)
            | Value::Bool(_)
            | Value::App(_)
            | Value::Xt(_)
            | Value::Mark(_)
            | Value::Failure(_)) => Err(other.kind()),
        }
    }
}

/// A list, of strings or of lists: what `concat` joins.
struct Joined;

impl Joined {
    fn wants(_: Builtin) -> Option<Wants> {
        Some(Wants::Concat)
    }

    fn of<A: AppValue>(value: Value<A>) -> Result<Rc<List<A>>, &'static str> {
        match value {
            Value::List(list) => Ok(list),
            other @ (Value::Int(_)
            | Value::Bool(_)
            | Value::Str(_)
            | Value::App(_)
            | Value::Xt(_)
            | Value::Mark(_)
            | Value::Failure(_)) => Err(other.kind()),
        }
    }
}

/// Runs `f` in a function of its own, never inlined where it is called.
#[inline(never)]
fn out_of_line<R>(f: impl FnOnce() -> R) -> R {
    f()
}

/// Expands rows `Variant "name" [flag] (arg: Marker, …) "effect" "about"
/// { body }` to the enum `Builtin` of those variants: `Builtin::ALL` in
/// that order; `name`, `effect` and `about`, each a match giving that row's
/// column; `immediate` and `hosted`, whether its row is flagged so (a
/// flagged row, `cold` too, runs out of line); and
/// `Machine::builtin`, which runs one. It takes as many values as the row
/// names (`take::<N>`; too few fail it), turns each into what its marker
/// gives (a value of another kind fails the word), and runs the row's body
/// with them bound to the row's names. In a body, `m` is the machine and
/// `host` its host, as the macro's first line names them.
///
/// `builtins! { m host; Len "len" (s: Seq) "seq -- n" "its length" { m.push(…) } }`
/// gives `Builtin::Len`, named "len", and in `builtin` the arm
/// `Builtin::Len => { let [s] = m.take()?; let s = Seq::of(s).map_err(…)?;
/// { m.push(…) } }`.
macro_rules! builtins {
    (@flag immediate immediate) => { true };
    (@flag hosted hosted) => { true };
    (@flag $want:ident $($got:ident)?) => { false };
    // A flagged row runs out of the step loop's line: its words shape a
    // program or are rare, and inlined they made the core's every step
    // slower (the dispatch past what the loop inlines).
    (@arm $arm:block) => { $arm };
    (@arm $arm:block $flag:ident) => { out_of_line(move || $arm) };
    ($m:ident $host:ident; $(
        $variant:ident $name:literal $([$flag:ident])? ($($arg:ident: $ty:ident),*) $effect:literal
        $about:literal $body:block
    )*) => {
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

            /// What it does to the stack, as help shows it.
            pub fn effect(self) -> &'static str {
                match self {
                    $(Builtin::$variant => $effect,)*
                }
            }

            /// What it does, in a line, as help says it: a word that reads
            /// the words after it says how it is written first.
            pub fn about(self) -> &'static str {
                match self {
                    $(Builtin::$variant => $about,)*
                }
            }

            /// Whether it runs even while a definition is compiled: the
            /// words that shape a program.
            pub fn immediate(self) -> bool {
                match self {
                    $(Builtin::$variant => builtins!(@flag immediate $($flag)?),)*
                }
            }

            /// Whether it is in the dictionary only where the host enables
            /// it (`Vocabulary::evaluate`).
            pub fn hosted(self) -> bool {
                match self {
                    $(Builtin::$variant => builtins!(@flag hosted $($flag)?),)*
                }
            }

            /// How many values it takes off the stack, for its effect to be
            /// held to; an immediate word takes its own.
            #[cfg(test)]
            pub(crate) fn takes(self) -> usize {
                match self {
                    $(Builtin::$variant => <[&str]>::len(&[$(stringify!($arg)),*]),)*
                }
            }

            /// The built-in `name` is, if one.
            pub fn named(name: &str) -> Option<Self> {
                match name {
                    $($name => Some(Builtin::$variant),)*
                    _ => None,
                }
            }
        }

        impl<A: AppValue> Machine<A> {
            /// Inlined in the step loop, its one caller.
            #[inline(always)]
            pub(crate) fn builtin<H: Host<Value = A>>(
                &mut self,
                word: Builtin,
                $host: &mut H,
            ) -> Result<(), Error> {
                let $m = self;
                match word {
                    $(Builtin::$variant => builtins!(@arm {
                        $m.depth(<[&str]>::len(&[$(stringify!($arg)),*]))?;
                        let [$($arg),*] = $m.moved();
                        $(let app = matches!($arg, Value::App(_));
                        let $arg = $ty::of($arg)
                            .map_err(|got| $m.refused($ty::wants(word), got, app))?;)*
                        $body
                    } $($flag)?),)*
                }
            }
        }
    };
}

builtins! { m host;
    Drop "drop" (_x: Any) "x --" "the top value dropped" {
        Ok(())
    }
    Pick "pick" (n: Int) "xn … x0 n -- xn … x0 xn"
        "a copy of the value n under the top (0 pick copies the top)" {
        let n = m.count(Builtin::Pick, n)?;
        m.pick(n)
    }
    Roll "roll" (n: Int) "xn … x0 n -- xn-1 … x0 xn"
        "the value n under the top moved to the top (1 roll swaps)" {
        let n = m.count(Builtin::Roll, n)?;
        m.roll(n)
    }
    Add "add" (a: Int, b: Int) "n m -- n+m" "integer arithmetic; past 64 bits it wraps around" {
        m.push(Value::Int(a.wrapping_add(b)));
        Ok(())
    }
    Sub "sub" (a: Int, b: Int) "n m -- n-m" "integer arithmetic; past 64 bits it wraps around" {
        m.push(Value::Int(a.wrapping_sub(b)));
        Ok(())
    }
    Mul "mul" (a: Int, b: Int) "n m -- n*m" "integer arithmetic; past 64 bits it wraps around" {
        m.push(Value::Int(a.wrapping_mul(b)));
        Ok(())
    }
    Div "div" (a: Int, b: Int) "n m -- n/m"
        "integer division, truncating toward zero; by zero it fails" {
        m.divided(a, b)
    }
    MulDivMod "mul-div-mod" [cold] (n: Int, k: Int, d: Int) "n k d -- r q"
        "n times k divided by d, the product kept exact: the remainder and the quotient, truncating toward zero; by zero it fails" {
        // Past 128 bits no product of two integers goes, and a remainder
        // is smaller than d; a quotient past 64 bits wraps around.
        let (product, d) = (i128::from(n).wrapping_mul(i128::from(k)), i128::from(d));
        let (Some(r), Some(q)) = (product.checked_rem(d), product.checked_div(d)) else {
            return Err(m.fail(Code::DivByZero, "division by zero"));
        };
        let r = i64::try_from(r).unwrap_or(0);
        let q = u64::try_from(q.cast_unsigned() & u128::from(u64::MAX)).unwrap_or(0);
        m.push(Value::Int(r));
        m.push(Value::Int(q.cast_signed()));
        Ok(())
    }
    And "and" [cold] (a: Any, b: Any) "a b -- x"
        "whether both are true (both worked out first); of two integers, the bits set in both" {
        m.logic(Builtin::And, (a, b), |a, b| a & b, |a, b| a & b)
    }
    Or "or" [cold] (a: Any, b: Any) "a b -- x"
        "whether either is true (both worked out first); of two integers, the bits set in either" {
        m.logic(Builtin::Or, (a, b), |a, b| a | b, |a, b| a | b)
    }
    Xor "xor" [cold] (a: Any, b: Any) "a b -- x"
        "whether one is true and the other not; of two integers, the bits set in one only" {
        m.logic(Builtin::Xor, (a, b), |a, b| a ^ b, |a, b| a ^ b)
    }
    ShiftLeft "shift-left" [cold] (n: Int, u: Int) "n u -- n"
        "n's bits moved u places up, 0s coming in (past 63 places, 0)" {
        let u = u32::try_from(m.count(Builtin::ShiftLeft, u)?).unwrap_or(u32::MAX);
        let moved = n.cast_unsigned().checked_shl(u).unwrap_or(0);
        m.push(Value::Int(moved.cast_signed()));
        Ok(())
    }
    ShiftRight "shift-right" [cold] (n: Int, u: Int) "n u -- n"
        "n's bits moved u places down, 0s coming in at the top (past 63 places, 0)" {
        let u = u32::try_from(m.count(Builtin::ShiftRight, u)?).unwrap_or(u32::MAX);
        let moved = n.cast_unsigned().checked_shr(u).unwrap_or(0);
        m.push(Value::Int(moved.cast_signed()));
        Ok(())
    }
    Within "within" [cold] (x: Int, lo: Int, hi: Int) "x lo hi -- flag"
        "whether x is from lo up to before hi; with hi below lo, whether it is outside hi up to before lo" {
        let inside = x.wrapping_sub(lo).cast_unsigned() < hi.wrapping_sub(lo).cast_unsigned();
        m.push(Value::Bool(inside));
        Ok(())
    }
    Lt "lt" (a: Any, b: Any) "a b -- a<b"
        "whether a comes before b: two integers, two strings or two lists" {
        let order = compare(&a, &b).ok_or_else(|| m.fail(Code::Type, unordered(&a, &b)))?;
        m.push(Value::Bool(order == Ordering::Less));
        Ok(())
    }
    Eq "eq" (a: Any, b: Any) "a b -- a=b" "whether two values are the same, of one kind" {
        m.push(Value::Bool(a == b));
        Ok(())
    }
    Len "len" (s: Seq) "seq -- n" "how many elements a string or list has" {
        m.push(Value::Int(i64::try_from(s.len()).unwrap_or(i64::MAX)));
        Ok(())
    }
    At "at" (s: Seq, i: Int) "seq i -- x"
        "element i, from 0 (of a string, its character's code)" {
        let len = s.len();
        let i = m.index(i, len)?;
        let item = match &s {
            Sequence::Str(s) => s.part(i, i.saturating_add(1)).chars().next()
                .map(|c| Value::Int(i64::from(u32::from(c)))),
            Sequence::List(list) => list.items().get(i).cloned(),
        };
        let item = item.ok_or_else(|| m.fail(Code::Range, format!("index {i}, and it has {len}")))?;
        m.push(item);
        Ok(())
    }
    Slice "slice" (s: Seq, from: Int, to: Int) "seq from to -- seq"
        "the elements from `from` up to before `to`" {
        let len = s.len();
        let bounds = usize::try_from(from).ok().zip(usize::try_from(to).ok());
        let Some((a, b)) = bounds.filter(|(a, b)| a <= b && *b <= len) else {
            return Err(m.fail(Code::Range, format!("slice from {from} to {to}, and it has {len}")));
        };
        let part = match &s {
            Sequence::Str(s) => Machine::text(s.part(a, b)),
            Sequence::List(list) => Machine::list(list.items().get(a..b).unwrap_or_default().to_vec()),
        };
        m.push(part);
        Ok(())
    }
    Concat "concat" (list: Joined) "list -- seq"
        "a list of strings joined into one string, or a list of lists into one list" {
        let joined = m.concat(&list)?;
        m.push(joined);
        Ok(())
    }
    Push "push" (s: Seq, x: Any) "seq x -- seq" "x added at the end (to a string, x is a string)" {
        let pushed = m.pushed(s, x)?;
        m.push(pushed);
        Ok(())
    }
    Put "put" (s: Seq, i: Int, x: Any) "seq i x -- seq"
        "element i replaced by x (in a string, by a string)" {
        let i = m.index(i, s.len())?;
        let put = m.put(s, i, x)?;
        m.push(put);
        Ok(())
    }
    Char "char" (n: Int) "c -- s" "the one-character string of the character with code c" {
        let c = m.character(n)?;
        let c = m.char(c);
        m.push(c);
        Ok(())
    }
    Depth "depth" [cold] () "-- n"
        "how many values the stack has (in list … end or try … catch, above where it began)" {
        let floor = m.marks.last().map_or(0, |(at, _)| *at);
        let n = m.stack.len().saturating_sub(floor);
        m.push(Value::Int(i64::try_from(n).unwrap_or(i64::MAX)));
        Ok(())
    }
    ToReturn "to-return" [cold] (x: Any) "x --" "x moved to the return stack, which holds values only" {
        m.returns.push(x);
        Ok(())
    }
    FromReturn "from-return" [cold] () "-- x" "the return stack's top value moved back" {
        m.returned()?;
        let x = m.returns.pop();
        m.stack.extend(x);
        Ok(())
    }
    CopyReturn "copy-return" [cold] () "-- x" "a copy of the return stack's top value" {
        m.returned()?;
        let x = m.returns.last().cloned();
        m.stack.extend(x);
        Ok(())
    }
    I "i" [cold] () "-- n" "in a do … loop: its index" {
        let index = m.running_loop(0, Builtin::I)?.index;
        m.push(Value::Int(index));
        Ok(())
    }
    J "j" [cold] () "-- n" "in a do … loop in another: the outer loop's index" {
        let index = m.running_loop(1, Builtin::J)?.index;
        m.push(Value::Int(index));
        Ok(())
    }
    Leave "leave" [cold] () "--" "in a do … loop: end it now, going on after its loop"
        { m.leave_loop() }
    DigitsBegin "digits-begin" [cold] () "--"
        "begin a number's text: digit, digits, hold, holds and sign put characters before it, and digits-end gives it" {
        m.digits.clear();
        Ok(())
    }
    Hold "hold" [cold] (n: Int) "c --" "the character with code c put before the text digits-end gives" {
        let c = m.character(n)?;
        m.digits.push(c);
        Ok(())
    }
    Holds "holds" [cold] (s: Str) "s --" "s put before the text digits-end gives" {
        m.digits.extend(s.chars().rev());
        Ok(())
    }
    DigitsEnd "digits-end" [cold] (_n: Int) "n -- s" "n dropped, and the text put together since digits-begin" {
        let text: String = m.digits.chars().rev().collect();
        m.digits.clear();
        m.push(Value::str(text));
        Ok(())
    }
    Kind "kind" (x: Any) "x -- name" "integer, boolean, string, list, xt, or the host's kind" {
        m.push(Machine::text(x.kind_name()));
        Ok(())
    }
    String "string" (x: Any) "x -- s" "x as text, as print shows it" {
        let text = x.text().map_err(|f| m.failed(f))?;
        m.push(Value::str(text));
        Ok(())
    }
    Print "print" (x: Any) "x --" "write x's text now, and a line end" {
        let mut text = x.text().map_err(|f| m.fail(f.code, f.message))?;
        text.push('\n');
        m.written(host, &text)
    }
    Write "write" [cold] (x: Any) "x --" "write x's text now, with no line end" {
        let text = x.text().map_err(|f| m.fail(f.code, f.message))?;
        m.written(host, &text)
    }
    Emit "emit" [cold] (n: Int) "c --" "write the character with code c now" {
        let c = m.character(n)?;
        m.written(host, c.encode_utf8(&mut [0; 4]))
    }
    Fail "fail" (x: Any) "x --" "stop the program, with x as the reason" {
        // Its reason is a string a try may keep: made as `string` makes one.
        let text = x.text().map_err(|f| m.fail(f.code, f.message))?;
        Err(m.fail(Code::Fail, text))
    }
    Throw "throw" [cold] (x: Any) "x --"
        "raise x, a failure, again; or the failure numbered x in Forth's table (0 raises none)"
        { m.throw(x) }
    Abort "abort" [cold] () "--" "stop the program: the failure abort, -1 throw" {
        m.throw(Value::Int(-1))
    }
    Quit "quit" [cold] () "--" "stop the program: the failure quit, -56 throw" {
        m.throw(Value::Int(-56))
    }
    Execute "execute" [cold] (xt: Exec) "xt --" "run the word xt is" {
        m.enter(&xt)
    }
    Find "find" [cold] (s: Str) "s -- xt true | s false"
        "the word named s, as the dictionary has it now, and true; else s and false" {
        m.find(s);
        Ok(())
    }
    NextWord "next-word" [cold] () "-- s" "the next word of the input, as written"
        { m.string(Builtin::NextWord) }
    Immediate "immediate" [cold] () "--" "the word defined last runs even while a definition is compiled"
        { m.immediate() }
    Compiling "compiling" [cold] () "-- flag" "whether the words read are being compiled" {
        m.push(Value::Bool(m.compiling));
        Ok(())
    }
    StartCompiling "start-compiling" [cold] () "--" "compile the words read again, into the definition open"
        { m.start_compiling() }
    Evaluate "evaluate" [hosted] (s: Str) "s --"
        "read s as a source file is read, and run it here" { m.evaluate(&s) }
    StopCompiling "stop-compiling" [immediate] () "--"
        "run the words read, until start-compiling, though a definition is open" {
        m.compiling = false;
        Ok(())
    }
    Str "str" [immediate] () "-- s" "str WORD: WORD as text, whatever it looks like"
        { m.string(Builtin::Str) }
    Define "define" [immediate] () "--"
        "define NAME … end: a word of your own, found from its end on (end closes code, list, each and try too)"
        { m.define() }
    End "end" [immediate] () "--" "… end: closes the innermost of define, code, list, each and try … catch"
        { m.end() }
    Code "code" [immediate] () "-- xt" "code … end: the words between, compiled, as an xt"
        { m.code() }
    Let "let" [immediate] () "x --"
        "x let NAME: NAME gives x from then on (in a definition, for that call)" { m.let_name() }
    To "to" [immediate] () "x --" "x to NAME: the let name NAME gives x from then on"
        { m.set_name() }
    Tick "tick" [immediate] () "-- xt" "tick NAME: the xt of the word NAME" { m.tick() }
    Postpone "postpone" [immediate] () "--"
        "postpone NAME: in a definition, what NAME does when compiled, done when the definition runs"
        { m.postpone() }
    Literal "literal" [immediate] (x: Any) "x --" "x literal: in a definition, x, pushed when the definition runs"
        { m.literal(x) }
    Recurse "recurse" [immediate] () "--" "in a definition: a call to the definition itself"
        { m.compile_only(Builtin::Recurse, Op::Recurse) }
    LeaveDefinition "leave-definition" [immediate] () "--"
        "in a definition: return from it now, its loops left"
        { m.compile_only(Builtin::LeaveDefinition, Op::Leave) }
    If "if" [immediate] () "flag --"
        "flag if … else … then: the first part if flag is true (or an integer not 0), else the second"
        { m.open(crate::reader::Construct::If) }
    Else "else" [immediate] () "--" "flag if … else … then: the part run when flag is false"
        { m.otherwise() }
    Then "then" [immediate] () "--" "flag if … else … then: closes it" { m.then() }
    Begin "begin" [immediate] () "--"
        "begin … flag until: the body until flag is true; begin … flag while … end, begin … again"
        { m.open(crate::reader::Construct::Begin) }
    Until "until" [immediate] () "flag --" "begin … flag until: closes it" { m.until() }
    While "while" [immediate] () "flag --"
        "begin … flag while … end: while flag is true, the part after while, then begin's again"
        { m.test_loop() }
    Again "again" [immediate] () "--"
        "begin … again: closes it, the body run for ever (until leave-definition or a failure)"
        { m.again() }
    Do "do" [immediate] () "limit first --"
        "limit first do … loop: the body for each index from first up to before limit, none when the two are equal; i gives it"
        { m.open(crate::reader::Construct::Do) }
    Loop "loop" [immediate] () "--" "limit first do … loop: closes it, the index 1 on each time"
        { m.close_loop(Op::Loop) }
    LoopBy "loop-by" [immediate] () "n --"
        "limit first do … n loop-by: closes it, the index n on each time, until it crosses from under the limit to it"
        { m.close_loop(Op::LoopBy) }
    Case "case" [immediate] () "x --"
        "x case v of … end … end: the part after the first v equal to x, x dropped; else the part before the last end, which drops x"
        { m.open(crate::reader::Construct::Case) }
    Of "of" [immediate] () "x v -- | x"
        "x case v of … end … end: when v equals x, x dropped and the part to of's end, then on after the case's"
        { m.of() }
    Ahead "ahead" [immediate] () "--" "ahead … then: a jump past the words up to then"
        { m.open(crate::reader::Construct::Ahead) }
    List "list" [immediate] () "-- list"
        "list … end: a list of the values its body leaves; the body starts with an empty stack"
        { m.open(crate::reader::Construct::List) }
    Each "each" [immediate] () "seq --"
        "seq each … end: the body once per element, the element pushed (a string's are character codes)"
        { m.open(crate::reader::Construct::Each) }
    Try "try" [immediate] () "codes --"
        "codes try … catch … end: the body from an empty stack, its values left; if it fails with one of codes (str 'timeout closed' just before try, str all for any), the handler instead, on the stack as it was at try and the failure"
        { m.try_body() }
    Catch "catch" [immediate] () "--" "codes try … catch … end: the handler" { m.catch() }
    Include "include" [immediate] () "--" "include PATH: the file's words read here, relative to the file naming it"
        { m.include(host, Builtin::Include) }
    Require "require" [immediate] () "--"
        "require PATH: the file's words read here, unless they were read already"
        { m.include(host, Builtin::Require) }
}
