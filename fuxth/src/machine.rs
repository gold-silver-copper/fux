//! Running a compiled program. All its state is data: the stack, a frame
//! for each call (the code and the step it is at), the `let` names, the
//! open `list`, `each` and `try` blocks. So it can stop anywhere and go on
//! later: after a budget of steps, so its host can look for Ctrl-C, and at
//! an application word that needs its host to do something first. It never
//! runs a program as Rust recursion, so no program can overflow the Rust
//! stack, and stopping is just dropping it.
use crate::check::{ANY, INT, Kinds, LIST, SEQ, STR};
use crate::compile::{Op, Program};
use crate::error::{Code, Error, Failure, Floor, Wants, underflow};
use crate::token::Token;
use crate::value::{AppValue, List, NESTING, TOTAL, Text, Value};
use crate::vocabulary::through;
use std::cmp::Ordering;
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

    /// Prints a line (`print`), now.
    fn print(&mut self, line: &str) -> Result<(), Failure>;
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
    /// The program ended.
    Finished,
    /// An application word waits for the host to do this.
    Suspended(R),
    /// The budget of steps ran out: run again to go on.
    Paused,
}

/// How far a program may go: values on the stack, calls nested.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub stack: usize,
    pub calls: usize,
}

impl Default for Limits {
    /// As many values on the stack as one list may hold, so `list … end`
    /// can gather any list there may be.
    fn default() -> Self {
        Self {
            stack: LONGEST,
            calls: 10_000,
        }
    }
}

/// The most elements one string or list a program builds may hold.
pub const LONGEST: usize = 1_000_000;

/// The most elements a run may build in all: what bounds its memory.
pub const BUILT: u64 = 16_000_000;

/// Elements a long step goes through for each step it costs.
const PER_STEP: u64 = 64;

/// What an application word is to give: it is checked once it has, now
/// or when the machine resumes. The stack's depth before it gave anything,
/// the word, and the kinds it took.
struct Given {
    depth: usize,
    word: usize,
    taken: Vec<crate::check::Kinds>,
}

/// A call under way: which code (`None`, the top level), its next step, and
/// where its `let` names start.
#[derive(Clone, Copy, Debug)]
struct Frame {
    code: Option<usize>,
    step: usize,
    /// For a call in last place, which took its caller's frame: the call
    /// that began the chain of such calls, as a code and the step after it.
    /// Only the first is kept, so a loop written as recursion keeps one.
    via: Option<(Option<usize>, usize)>,
    locals: usize,
}

/// An open block whose body starts with an empty stack: a `list`, or a
/// `try` with the codes it catches, its handler's step, and the frames
/// and `each` loops there were at it, to go back to.
enum Body {
    List,
    Try {
        codes: Rc<Vec<Code>>,
        handler: usize,
        frames: usize,
        iters: usize,
    },
}

/// A program being run.
pub struct Machine<A> {
    program: Rc<Program<A>>,
    stack: Vec<Value<A>>,
    frames: Vec<Frame>,
    /// Every frame's `let` names, each frame's from its `locals`.
    locals: Vec<Option<Value<A>>>,
    globals: Vec<Option<Value<A>>>,
    /// Where each open `list` or `try` body began on the stack, and which:
    /// nothing below may be taken inside it.
    marks: Vec<(usize, Body)>,
    /// Each open `each`: what it walks, and where its next element is: an
    /// index in a list, a byte in a string (so each step finds it at once).
    iters: Vec<(Sequence<A>, usize)>,
    limits: Limits,
    /// What application words give, kept between calls.
    out: Vec<Value<A>>,
    /// A suspended application word: what it is to give.
    pending: Option<Given>,
    /// Steps the last long step costs beyond its own.
    extra: u64,
    /// Steps taken in all, through every run, long steps' costs counted.
    steps: u64,
    /// Elements built so far.
    built: u64,
    /// The step taken last: how many frames there were, its code, and its
    /// place in it.
    last: Option<(usize, Option<usize>, usize)>,
}

impl<A: AppValue> Machine<A> {
    pub fn new(program: Program<A>, limits: Limits) -> Self {
        let globals = vec![None; program.globals];
        Self {
            program: Rc::new(program),
            stack: Vec::new(),
            frames: vec![Frame {
                code: None,
                step: 0,
                via: None,
                locals: 0,
            }],
            // The top level keeps no `let` names of its own: its are globals.
            locals: Vec::new(),
            globals,
            marks: Vec::new(),
            iters: Vec::new(),
            limits,
            out: Vec::new(),
            pending: None,
            extra: 0,
            steps: 0,
            built: 0,
            last: None,
        }
    }

    /// The stack, deepest first.
    pub fn stack(&self) -> &[Value<A>] {
        &self.stack
    }

    /// Pushes a value: a suspended word's result.
    pub fn push(&mut self, value: Value<A>) -> Result<(), Error> {
        if self.stack.len() >= self.limits.stack {
            return Err(self.fail(Code::StackOverflow, "the stack is full"));
        }
        self.stack.push(value);
        Ok(())
    }

    /// Raises a suspended word's failure at it, as if the word had failed
    /// when called: a `try` around it that catches its code goes on at its
    /// handler, when the machine runs again; else its error, said at the
    /// word, ends the run.
    pub fn raise(&mut self, failure: Failure) -> Result<(), Error> {
        self.pending = None;
        let error = self.failed(failure);
        self.recover(error)
    }

    /// An application word's failure, said at the word, or the word it
    /// names.
    fn failed(&self, failure: Failure) -> Error {
        let mut error = self.fail(failure.code, failure.message);
        error.word = failure.word.or(error.word);
        error
    }

    /// `error` caught by the innermost `try` that names its code: what the
    /// body began undone (its calls, `let` frames, blocks and values), its
    /// message and code pushed, the handler next. Else `error`.
    fn recover(&mut self, mut error: Error) -> Result<(), Error> {
        loop {
            let catches = |(_, body): &(usize, Body)| matches!(body, Body::Try { codes, .. } if codes.contains(&error.code));
            let Some(caught) = self.marks.iter().rposition(catches) else {
                return Err(error);
            };
            // Those inside it end with it.
            self.marks.truncate(caught.saturating_add(1));
            let Some((
                depth,
                Body::Try {
                    handler,
                    frames,
                    iters,
                    ..
                },
            )) = self.marks.pop()
            else {
                return Err(error);
            };
            while self.frames.len() > frames {
                self.leave();
            }
            self.stack.truncate(depth);
            self.iters.truncate(iters);
            self.jump(handler);
            let code = Value::str(error.code.name());
            // A stack full to its limit fails again, for a `try` further
            // out to catch.
            match (self.push(Value::str(error.message))).and_then(|()| self.push(code)) {
                Ok(()) => return Ok(()),
                Err(again) => error = again,
            }
        }
    }

    /// The step each call under way is at, the outermost first: the word
    /// that called each definition, down to the word it ran last.
    /// A call in last place, which took its caller's frame, still shows
    /// the call that began it.
    pub fn calls(&self) -> Vec<Token> {
        let mut calls: Vec<Token> = (self.frames.iter())
            .flat_map(|frame| {
                let via = frame.via.and_then(|(code, step)| self.before(code, step));
                via.into_iter().chain(self.before(frame.code, frame.step))
            })
            .collect();
        // The innermost: the step it really took last, a jump's too.
        if let Some(last) = self.taken() {
            if (self.frames.last()).is_some_and(|f| self.before(f.code, f.step).is_some()) {
                calls.pop();
            }
            calls.push(last);
        }
        calls
    }

    /// The token of the step the innermost frame took last: none for a
    /// call just begun (its caller took the call).
    fn taken(&self) -> Option<Token> {
        let (depth, code, step) = self.last?;
        let frame = self.frames.last()?;
        let begun = frame.step == 0 && code != frame.code;
        let ours = depth == self.frames.len() && !begun;
        ours.then(|| self.program.token(code, step).cloned())?
    }

    /// The token the step just taken came from: where an error is, and
    /// the word a suspension is for, which the host's failure in doing it
    /// may name. In a frame at its first step (a call just begun, a loop
    /// just gone back), the call that began it, else the step about to be
    /// taken.
    pub fn at(&self) -> Option<Token> {
        if let Some(taken) = self.taken() {
            return Some(taken);
        }
        let frame = self.frames.last()?;
        self.before(frame.code, frame.step)
            .or_else(|| frame.via.and_then(|(code, step)| self.before(code, step)))
            .or_else(|| self.program.token(frame.code, frame.step).cloned())
    }

    /// The token of the step before `step` in `code`: none at its first.
    fn before(&self, code: Option<usize>, step: usize) -> Option<Token> {
        self.program.token(code, step.checked_sub(1)?).cloned()
    }

    fn fail(&self, code: Code, message: impl Into<String>) -> Error {
        Error::new(code, message, self.at().as_ref())
    }

    /// Ends the frame on top: its `let` names go with it.
    fn leave(&mut self) {
        if let Some(frame) = self.frames.pop() {
            self.locals.truncate(frame.locals);
        }
    }

    /// Steps taken so far, in every run, a long step counted as the steps
    /// it costs.
    pub fn steps(&self) -> u64 {
        self.steps
    }

    /// Runs up to `budget` steps.
    pub fn run<H: Host<Value = A>>(
        &mut self,
        host: &mut H,
        budget: u64,
    ) -> Result<Outcome<H::Request>, Error> {
        if let Some(given) = self.pending.take() {
            self.given(given)?;
        }
        let until = self.steps.saturating_add(budget);
        loop {
            match self.steps_until(host, until) {
                Err(error) => self.recover(error)?,
                ran => return ran,
            }
        }
    }

    /// Runs until `until` steps are taken in all, or a step fails.
    fn steps_until<H: Host<Value = A>>(
        &mut self,
        host: &mut H,
        until: u64,
    ) -> Result<Outcome<H::Request>, Error> {
        let program = Rc::clone(&self.program);
        loop {
            if self.steps >= until {
                return Ok(Outcome::Paused);
            }
            self.steps = (self.steps)
                .saturating_add(1)
                .saturating_add(std::mem::take(&mut self.extra));
            let depth = self.frames.len();
            let Some(frame) = self.frames.last_mut() else {
                return Ok(Outcome::Finished);
            };
            let Some(code) = program.code(frame.code) else {
                self.leave();
                continue;
            };
            // Past its last step, a definition returns; the program ends,
            // and none of what is left must be used.
            let Some(op) = code.ops.get(frame.step) else {
                if depth == 1 {
                    self.used()?;
                }
                self.leave();
                continue;
            };
            self.last = Some((depth, frame.code, frame.step));
            frame.step = frame.step.saturating_add(1);
            match op {
                Op::Push(value) => self.push(value.clone())?,
                Op::Builtin(builtin) => self.builtin(*builtin, host)?,
                Op::PickN(n) => self.pick(*n)?,
                Op::RollN(n) => self.roll(*n)?,
                Op::App(id) => {
                    if let Some(request) = self.app(*id, host)? {
                        return Ok(Outcome::Suspended(request));
                    }
                }
                Op::Call(id) => {
                    // A call that is its caller's last step replaces it:
                    // the caller has nothing left to do, so a loop written
                    // as recursion runs in one frame.
                    let mut via = None;
                    if returns_from(&code.ops, frame.step) {
                        via = Some(frame.via.unwrap_or((frame.code, frame.step)));
                        self.leave();
                    } else if self.frames.len() > self.limits.calls {
                        return Err(self.too_deep());
                    }
                    let base = self.locals.len();
                    let slots = program.words.get(*id).map_or(0, |w| w.locals);
                    self.locals.resize(base.saturating_add(slots), None);
                    self.frames.push(Frame {
                        code: Some(*id),
                        step: 0,
                        via,
                        locals: base,
                    });
                }
                Op::Jump(to) => frame.step = *to,
                Op::JumpUnless(to) => {
                    if !self.flag()? {
                        self.jump(*to);
                    }
                }
                Op::SetLocal(slot) => {
                    let at = frame.locals.saturating_add(*slot);
                    let value = self.pop()?;
                    if let Some(local) = self.locals.get_mut(at) {
                        *local = Some(value);
                    }
                }
                Op::GetLocal(slot) => {
                    let at = frame.locals.saturating_add(*slot);
                    let value = self.locals.get(at).cloned().flatten();
                    self.named(value)?;
                }
                Op::SetGlobal(slot) => {
                    let value = self.pop()?;
                    if let Some(global) = self.globals.get_mut(*slot) {
                        *global = Some(value);
                    }
                }
                Op::GetGlobal(slot) => {
                    let value = self.globals.get(*slot).cloned().flatten();
                    self.named(value)?;
                }
                Op::Mark => self.marks.push((self.stack.len(), Body::List)),
                Op::Gather => {
                    let from = self
                        .marks
                        .pop()
                        .map_or(0, |(at, _)| at)
                        .min(self.stack.len());
                    let mut items = Vec::with_capacity(self.stack.len().saturating_sub(from));
                    while self.stack.len() > from {
                        items.extend(self.stack.pop());
                    }
                    items.reverse();
                    let list = self.list(items)?;
                    self.push(list)?;
                }
                Op::EachStart => {
                    let walked = self.pop()?;
                    let app = matches!(walked, Value::App(_));
                    match Seq::of(walked) {
                        Ok(seq) => self.iters.push((seq, 0)),
                        Err(kind) => return Err(self.fail(Code::Type, Wants::Each.not(kind, app))),
                    }
                }
                Op::EachNext(out) => {
                    let next = (self.iters.last_mut()).and_then(|(seq, at)| next(seq, at));
                    match next {
                        Some(item) => self.push(item)?,
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
                    };
                    self.marks.push((self.stack.len(), body));
                }
                Op::Tried(out) => {
                    self.marks.pop();
                    frame.step = *out;
                }
            }
        }
    }

    /// Application word `id` run on the stack's top values: the request
    /// to wait for, if it suspends.
    fn app<H: Host<Value = A>>(
        &mut self,
        id: usize,
        host: &mut H,
    ) -> Result<Option<H::Request>, Error> {
        let words = &self.program.vocabulary;
        let from = self.depth(words.word(id).map_or(0, |w| w.takes.len()))?;
        // The kinds it takes, for what it passes through.
        let taken = self
            .stack
            .iter()
            .skip(from)
            .map(|v| words.kind_of(v))
            .collect();
        let mut out = std::mem::take(&mut self.out);
        out.clear();
        let args = Args {
            stack: &mut self.stack,
            from,
            at: from,
        };
        let done = host.call(id, args, &mut out).map_err(|f| self.failed(f))?;
        let given = Given {
            depth: self.stack.len(),
            word: id,
            taken,
        };
        if self.stack.len().saturating_add(out.len()) > self.limits.stack {
            return Err(self.fail(Code::StackOverflow, "the stack is full"));
        }
        self.stack.append(&mut out);
        self.out = out;
        match done {
            Done::Now => self.given(given).map(|()| None),
            // What it gives comes with the host's answer.
            Done::Later(request) => {
                self.pending = Some(given);
                Ok(Some(request))
            }
        }
    }

    /// A `let` name's value pushed; `unset` if its `let` hasn't run.
    fn named(&mut self, value: Option<Value<A>>) -> Result<(), Error> {
        match value {
            Some(value) => self.push(value),
            None => Err(self.fail(Code::Unset, "read before its let ran")),
        }
    }

    /// Whether an application word gave as many values as its vocabulary
    /// says, of the kinds it says: the checker counted on it. Kept out of
    /// `run`'s loop: inlined there, it made every program 2 to 5% slower
    /// (fuxth's bench).
    #[inline(never)]
    fn given(&self, given: Given) -> Result<(), Error> {
        let words = &self.program.vocabulary;
        let gives = words.word(given.word).map_or(&[][..], |w| &w.gives);
        let values = self.stack.get(given.depth..).unwrap_or_default();
        if values.len() != gives.len() {
            let message = format!(
                "the application gave {} values, and its word says {}",
                values.len(),
                gives.len()
            );
            return Err(self.fail(Code::Contract, message));
        }
        for (value, (from, map)) in values.iter().zip(gives) {
            let taken = from.and_then(|i| given.taken.get(i).copied());
            let may = through(map, taken.unwrap_or(ANY));
            let kind = words.kind_of(value);
            if !kind.may_be(may) {
                let (kind, may) = (words.describe(kind), words.describe(may));
                let message = format!("the application gave {kind}, and its word says {may}");
                return Err(self.fail(Code::Contract, message));
            }
        }
        Ok(())
    }

    /// At the program's end: a value left that must be used fails it.
    fn used(&self) -> Result<(), Error> {
        let words = &self.program.vocabulary;
        if let Some(why) = self
            .stack
            .iter()
            .find_map(|v| words.unused(words.kind_of(v)))
        {
            return Err(self.fail(Code::Unused, why));
        }
        Ok(())
    }

    /// Calls nested past the limit: said at the call to the word that
    /// calls itself, the innermost such (not one it called last, which may
    /// be a library's), naming the word it called last.
    #[cold]
    fn too_deep(&self) -> Error {
        let message = format!("calls nest past {}: recursion too deep", self.limits.calls);
        let error = self.fail(Code::CallDepth, message);
        let frames = &self.frames;
        let call = (1..frames.len()).rev().find_map(|i| {
            let (caller, frame) = (frames.get(i.checked_sub(1)?)?, frames.get(i)?);
            let again = frames.iter().take(i).any(|f| f.code == frame.code);
            again.then(|| self.before(caller.code, caller.step))?
        });
        let last = error.word.as_deref().unwrap_or_default();
        match call.filter(|call| *call.text != *last) {
            Some(call) => Error::new(
                Code::CallDepth,
                format!("{} (in {last})", error.message),
                Some(&call),
            ),
            None => error,
        }
    }

    fn jump(&mut self, to: usize) {
        if let Some(frame) = self.frames.last_mut() {
            frame.step = to;
        }
    }

    /// Where the top `n` values start, if the stack has them: inside a
    /// `list`, only those above where it began.
    fn depth(&self, n: usize) -> Result<usize, Error> {
        let floor = self.marks.last().map_or(0, |(at, _)| *at);
        let has = self.stack.len().saturating_sub(floor);
        if has >= n {
            return Ok(self.stack.len().saturating_sub(n));
        }
        let inside = self.marks.last().map(|(_, body)| match body {
            Body::List => Floor::List,
            Body::Try { .. } => Floor::Try,
        });
        let message = underflow("takes", n, &has.to_string(), inside);
        Err(self.fail(Code::StackUnderflow, message))
    }

    fn pop(&mut self) -> Result<Value<A>, Error> {
        let [value] = self.take()?;
        Ok(value)
    }

    /// Takes a boolean, as `if` and `until` do.
    fn flag(&mut self) -> Result<bool, Error> {
        match self.pop()? {
            Value::Bool(flag) => Ok(flag),
            other @ (Value::Int(_) | Value::Str(_) | Value::List(_) | Value::App(_)) => {
                Err(self.fail(Code::Type, Wants::Flag.not(other.kind(), false)))
            }
        }
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

    /// `word` given `got` in a place that takes `arg`'s kinds.
    #[cold]
    fn refused(&self, word: Builtin, arg: Arg, got: &str, app: bool) -> Error {
        // A place that takes any value refuses none.
        let wants = arg.wants.map(|wants| wants(word));
        self.fail(
            Code::Type,
            wants.map_or_else(|| got.to_owned(), |wants| wants.not(got, app)),
        )
    }

    /// A built-in's `N` values off the stack, the deepest first. Too few
    /// are said as the word takes them, before any is taken.
    fn take<const N: usize>(&mut self) -> Result<[Value<A>; N], Error> {
        self.depth(N)?;
        // The stack has them: none is the `false` put for one it lacks.
        let mut taken = std::array::from_fn(|_| self.stack.pop().unwrap_or(Value::Bool(false)));
        taken.reverse();
        Ok(taken)
    }

    /// Charges a long step for going through `n` elements.
    fn cost(&mut self, n: usize) {
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        self.extra = self.extra.saturating_add(n / PER_STEP);
    }

    /// Counts `n` elements built: past what a run may build in all, or past
    /// what one value may hold, a `too_long` error.
    fn build(&mut self, n: usize) -> Result<(), Error> {
        self.fits(n)?;
        self.cost(n);
        self.built = self
            .built
            .saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
        if self.built > BUILT {
            return Err(self.fail(
                Code::TooLong,
                format!("built more than {BUILT} elements in all"),
            ));
        }
        Ok(())
    }

    /// A list of `items`, counted as built.
    fn list(&mut self, items: Vec<Value<A>>) -> Result<Value<A>, Error> {
        self.build(items.len())?;
        match List::new(items) {
            Some(list) => Ok(Value::List(Rc::new(list))),
            None => Err(self.nested()),
        }
    }

    fn nested(&self) -> Error {
        self.fail(Code::TooLong, format!(
            "lists nest at most {} deep, and hold at most {} in all, through the lists and strings in them",
            NESTING,
            TOTAL
        ))
    }

    /// Charges a step that walks a value through for its weight.
    fn walk(&mut self, value: &Value<A>) {
        let n = usize::try_from(value.weight()).unwrap_or(usize::MAX);
        self.cost(n);
    }

    fn pick(&mut self, n: usize) -> Result<(), Error> {
        let from = self.depth(n.saturating_add(1))?;
        match self.stack.get(from).cloned() {
            Some(value) => self.push(value),
            None => Err(self.fail(Code::StackUnderflow, "pick reached past the stack")),
        }
    }

    /// Arithmetic's result pushed, `None` if there is none: past what an
    /// integer holds, or a division by zero (`b` 0).
    fn arithmetic(&mut self, result: Option<i64>, b: i64) -> Result<(), Error> {
        match result {
            Some(n) => self.push(Value::Int(n)),
            None if b == 0 => Err(self.fail(Code::DivByZero, "division by zero")),
            None => Err(self.fail(Code::Overflow, "past what an integer holds")),
        }
    }

    fn roll(&mut self, n: usize) -> Result<(), Error> {
        let from = self.depth(n.saturating_add(1))?;
        if let Some(values) = self.stack.get_mut(from..) {
            values.rotate_left(1);
        }
        self.cost(n);
        Ok(())
    }

    fn concat(&mut self, list: &List<A>) -> Result<Value<A>, Error> {
        let mut text = String::new();
        let mut items = Vec::new();
        let (mut strings, mut lists) = (false, false);
        for item in list.items() {
            match item {
                Value::Str(s) => {
                    strings = true;
                    text.push_str(s);
                }
                Value::List(l) => {
                    lists = true;
                    items.extend(l.items().iter().cloned());
                }
                Value::Int(_) | Value::Bool(_) | Value::App(_) => {
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
            let text = Text::new(text);
            self.build(text.chars_len())?;
            Ok(Value::Str(Rc::new(text)))
        } else {
            self.list(items)
        }
    }

    fn pushed(&mut self, seq: Sequence<A>, x: Value<A>) -> Result<Value<A>, Error> {
        match (seq, x) {
            (Sequence::List(mut list), x) => {
                self.fits(list.items().len().saturating_add(1))?;
                self.build(copied(&list, list.items().len()).saturating_add(1))?;
                // In place when nothing else holds the list.
                match Rc::make_mut(&mut list).push(x) {
                    Some(()) => Ok(Value::List(list)),
                    None => Err(self.nested()),
                }
            }
            (Sequence::Str(mut s), Value::Str(x)) => {
                self.fits(s.chars_len().saturating_add(x.chars_len()))?;
                self.build(copied(&s, s.chars_len()).saturating_add(x.chars_len()))?;
                Rc::make_mut(&mut s).append(&x);
                Ok(Value::Str(s))
            }
            (Sequence::Str(_), x) => {
                let app = matches!(x, Value::App(_));
                Err(self.fail(Code::Type, Wants::PushText.not(x.kind(), app)))
            }
        }
    }

    /// Whether a string or list `n` elements long may be.
    fn fits(&self, n: usize) -> Result<(), Error> {
        if n > LONGEST {
            return Err(self.fail(
                Code::TooLong,
                format!("{n} elements, and a string or list holds at most {LONGEST}"),
            ));
        }
        Ok(())
    }

    fn put(&mut self, seq: Sequence<A>, i: usize, x: Value<A>) -> Result<Value<A>, Error> {
        match (seq, x) {
            (Sequence::List(mut list), x) => {
                let others = list.items().len().saturating_sub(1);
                self.build(copied(&list, others).saturating_add(1))?;
                match Rc::make_mut(&mut list).put(i, x) {
                    Some(looked) => {
                        self.cost(looked);
                        Ok(Value::List(list))
                    }
                    None => Err(self.nested()),
                }
            }
            (Sequence::Str(mut s), Value::Str(x)) => {
                // One character replaced by x's: the string is made anew,
                // in place or not.
                let others = s.chars_len().saturating_sub(1);
                self.fits(others.saturating_add(x.chars_len()))?;
                self.build(copied(&s, others).saturating_add(x.chars_len()))?;
                self.cost(others);
                Rc::make_mut(&mut s).replace(i, &x);
                Ok(Value::Str(s))
            }
            (Sequence::Str(_), x) => {
                let app = matches!(x, Value::App(_));
                Err(self.fail(Code::Type, Wants::PutText.not(x.kind(), app)))
            }
        }
    }
}

/// What changing a value held in `rc` copies first, of its `n` elements
/// that stay: all of them when it is held elsewhere too, else none (it is
/// changed in place). What is copied is built.
fn copied<T>(rc: &Rc<T>, n: usize) -> usize {
    if Rc::strong_count(rc) > 1 { n } else { 0 }
}

/// The element of `seq` at `at` (a byte in a string, an index in a list),
/// and `at` moved past it.
fn next<A: Clone>(seq: &Sequence<A>, at: &mut usize) -> Option<Value<A>> {
    match seq {
        Sequence::Str(s) => {
            let c = s.get(*at..).and_then(|rest| rest.chars().next());
            *at = at.saturating_add(c.map_or(1, char::len_utf8));
            c.map(|c| Value::str(c.to_string()))
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
    // As deep as lists nest, at most `NESTING`.
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
/// `None` for any other two. As deep as lists nest, at most `NESTING`.
fn compare<A: PartialEq>(a: &Value<A>, b: &Value<A>) -> Option<Ordering> {
    match (a, b) {
        (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
        (Value::Str(a), Value::Str(b)) => Some(a.as_ref().cmp(b.as_ref())),
        (Value::List(a), Value::List(b)) => {
            for (x, y) in a.items().iter().zip(b.items()) {
                if x != y {
                    return compare(x, y);
                }
            }
            Some(a.items().len().cmp(&b.items().len()))
        }
        (Value::Int(_) | Value::Bool(_) | Value::Str(_) | Value::List(_) | Value::App(_), _) => {
            None
        }
    }
}

/// A string or a list: what the sequence words take, and `each` walks.
#[derive(Clone, Debug)]
pub(crate) enum Sequence<A> {
    Str(Rc<Text>),
    List(Rc<List<A>>),
}

impl<A> Sequence<A> {
    /// How many elements it has.
    fn len(&self) -> usize {
        match self {
            Self::Str(s) => s.chars_len(),
            Self::List(list) => list.items().len(),
        }
    }
}

/// What a built-in takes in one place, as the checker sees it: the kinds
/// a value there may be, and what a value of another kind is told (none
/// for a place that takes any value).
#[derive(Clone, Copy)]
pub(crate) struct Arg {
    pub(crate) kinds: Kinds,
    pub(crate) wants: Option<fn(Builtin) -> Wants>,
}

/// The kinds of value a built-in's place takes, each a marker: its `ARG`,
/// for the checker, and `of`, which turns a value into what the word's
/// body takes, or says what it is instead, for `ARG.wants` to tell.
struct Any;

impl Any {
    const ARG: Arg = Arg {
        kinds: ANY,
        wants: None,
    };

    fn of<A>(value: Value<A>) -> Result<Value<A>, &'static str> {
        Ok(value)
    }
}

/// An integer.
struct Int;

impl Int {
    const ARG: Arg = Arg {
        kinds: INT,
        wants: Some(Wants::Int),
    };

    fn of<A: AppValue>(value: Value<A>) -> Result<i64, &'static str> {
        match value {
            Value::Int(n) => Ok(n),
            other @ (Value::Bool(_) | Value::Str(_) | Value::List(_) | Value::App(_)) => {
                Err(other.kind())
            }
        }
    }
}

/// A string or a list.
struct Seq;

impl Seq {
    const ARG: Arg = Arg {
        kinds: SEQ,
        wants: Some(Wants::Seq),
    };

    fn of<A: AppValue>(value: Value<A>) -> Result<Sequence<A>, &'static str> {
        match value {
            Value::Str(s) => Ok(Sequence::Str(s)),
            Value::List(list) => Ok(Sequence::List(list)),
            other @ (Value::Int(_) | Value::Bool(_) | Value::App(_)) => Err(other.kind()),
        }
    }
}

/// A list, of strings or of lists: what `concat` joins.
struct Joined;

impl Joined {
    const ARG: Arg = Arg {
        kinds: LIST,
        wants: Some(|_| Wants::Concat),
    };

    fn of<A: AppValue>(value: Value<A>) -> Result<Rc<List<A>>, &'static str> {
        match value {
            Value::List(list) => Ok(list),
            other @ (Value::Int(_) | Value::Bool(_) | Value::Str(_) | Value::App(_)) => {
                Err(other.kind())
            }
        }
    }
}

/// A one-character string, as its character. The checker sees a string.
struct Char;

impl Char {
    const ARG: Arg = Arg {
        kinds: STR,
        wants: Some(|_| Wants::Char),
    };

    fn of<A: AppValue>(value: Value<A>) -> Result<char, &'static str> {
        match &value {
            Value::Str(s) => {
                let mut chars = s.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => Ok(c),
                    (None, _) => Err("an empty one"),
                    (Some(_), Some(_)) => Err("a longer one"),
                }
            }
            Value::Int(_) | Value::Bool(_) | Value::List(_) | Value::App(_) => Err(value.kind()),
        }
    }
}

/// Expands rows `Variant "name" (arg: Marker, …) "effect" "about" { body }`
/// to the enum `Builtin` of those variants: `Builtin::ALL` in that order;
/// `name`, `effect` and `about`, each a match giving that row's column;
/// `args`, its places' markers' `ARG`s, the deepest first (as many as it
/// takes); and `Machine::builtin`, which runs one. It takes as many values
/// as the row names (`take::<N>`; too few fail it), turns each into what its
/// marker gives (a value of another kind fails the word), and runs the
/// row's body with them bound to the row's names. In a body, `m` is the
/// machine and `host` its host, as the macro's first line names them.
///
/// `builtins! { m host; Len "len" (s: Seq) "seq -- n" "its length" { m.push(…) } }`
/// gives `Builtin::Len`, named "len", whose `args` are `[Seq::ARG]`, and
/// in `builtin` the arm `Builtin::Len => { let [s] = m.take()?; let s =
/// Seq::of(s).map_err(…)?; { m.push(…) } }`.
macro_rules! builtins {
    ($m:ident $host:ident; $(
        $variant:ident $name:literal ($($arg:ident: $ty:ident),*) $effect:literal
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

            /// What it does, in a line, as help says it.
            pub fn about(self) -> &'static str {
                match self {
                    $(Builtin::$variant => $about,)*
                }
            }

            /// What it takes in each place, the deepest first: `pick` and
            /// `roll` take a count, then as many values as it says (the
            /// machine checks those).
            pub(crate) fn args(self) -> &'static [Arg] {
                match self {
                    $(Builtin::$variant => &[$($ty::ARG),*],)*
                }
            }
        }

        impl<A: AppValue> Machine<A> {
            fn builtin<H: Host<Value = A>>(
                &mut self,
                word: Builtin,
                $host: &mut H,
            ) -> Result<(), Error> {
                let $m = self;
                match word {
                    $(Builtin::$variant => {
                        let [$($arg),*] = $m.take()?;
                        $(let app = matches!($arg, Value::App(_));
                        let $arg = $ty::of($arg)
                            .map_err(|got| $m.refused(word, $ty::ARG, got, app))?;)*
                        $body
                    })*
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
    Add "add" (a: Int, b: Int) "n m -- n+m" "integer arithmetic, checked" {
        m.arithmetic(a.checked_add(b), b)
    }
    Sub "sub" (a: Int, b: Int) "n m -- n-m" "integer arithmetic, checked" {
        m.arithmetic(a.checked_sub(b), b)
    }
    Mul "mul" (a: Int, b: Int) "n m -- n*m" "integer arithmetic, checked" {
        m.arithmetic(a.checked_mul(b), b)
    }
    Div "div" (a: Int, b: Int) "n m -- n/m" "integer division, checked; it truncates toward zero" {
        m.arithmetic(a.checked_div(b), b)
    }
    Lt "lt" (a: Any, b: Any) "a b -- a<b"
        "whether a comes before b: two integers, two strings or two lists" {
        m.walk(if a.weight() < b.weight() { &a } else { &b });
        match compare(&a, &b) {
            Some(order) => m.push(Value::Bool(order == Ordering::Less)),
            None => Err(m.fail(Code::Type, unordered(&a, &b))),
        }
    }
    Eq "eq" (a: Any, b: Any) "a b -- a=b" "whether two values are the same, of one kind" {
        m.walk(if a.weight() < b.weight() { &a } else { &b });
        m.push(Value::Bool(a == b))
    }
    Len "len" (s: Seq) "seq -- n" "how many elements a string or list has" {
        m.push(Value::Int(i64::try_from(s.len()).unwrap_or(i64::MAX)))
    }
    At "at" (s: Seq, i: Int) "seq i -- x"
        "element i, from 0 (of a string, a one-character string)" {
        let len = s.len();
        let i = m.index(i, len)?;
        let item = match &s {
            Sequence::Str(s) => {
                if !s.is_ascii() {
                    m.cost(i);
                }
                Some(Value::str(s.part(i, i.saturating_add(1))))
            }
            Sequence::List(list) => list.items().get(i).cloned(),
        };
        match item {
            Some(item) => m.push(item),
            None => Err(m.fail(Code::Range, format!("index {i}, and it has {len}"))),
        }
    }
    Slice "slice" (s: Seq, from: Int, to: Int) "seq from to -- seq"
        "the elements from `from` up to before `to`" {
        let len = s.len();
        let bounds = usize::try_from(from).ok().zip(usize::try_from(to).ok());
        let Some((a, b)) = bounds.filter(|(a, b)| a <= b && *b <= len) else {
            return Err(m.fail(Code::Range, format!("slice from {from} to {to}, and it has {len}")));
        };
        let n = b.saturating_sub(a);
        let part = match &s {
            Sequence::Str(s) => {
                // Characters not all ASCII are walked to `b`.
                if !s.is_ascii() {
                    m.cost(a);
                }
                m.build(n)?;
                Value::str(s.part(a, b))
            }
            Sequence::List(list) => {
                let items = list.items().iter().skip(a).take(n).cloned().collect();
                m.list(items)?
            }
        };
        m.push(part)
    }
    Concat "concat" (list: Joined) "list -- seq"
        "a list of strings joined into one string, or a list of lists into one list" {
        let joined = m.concat(&list)?;
        m.push(joined)
    }
    Push "push" (s: Seq, x: Any) "seq x -- seq" "x added at the end (to a string, x is a string)" {
        let pushed = m.pushed(s, x)?;
        m.push(pushed)
    }
    Put "put" (s: Seq, i: Int, x: Any) "seq i x -- seq"
        "element i replaced by x (in a string, by a string)" {
        let i = m.index(i, s.len())?;
        let put = m.put(s, i, x)?;
        m.push(put)
    }
    Code "code" (c: Char) "c -- n" "the character code of a one-character string" {
        m.push(Value::Int(i64::from(u32::from(c))))
    }
    Char "char" (n: Int) "n -- c" "the one-character string with code n" {
        match u32::try_from(n).ok().and_then(char::from_u32) {
            Some(c) => m.push(Value::str(c.to_string())),
            None => Err(m.fail(Code::Range, format!("{n} is no character code"))),
        }
    }
    Kind "kind" (x: Any) "x -- name" "integer, boolean, string, list, or the host's kind" {
        m.push(Value::str(x.kind_name()))
    }
    String "string" (x: Any) "x -- s" "x as text, as print shows it" {
        m.walk(&x);
        // Made only as far as a string may be long.
        let Some(text) = x.text_upto(LONGEST) else {
            return Err(m.fail(Code::TooLong, format!(
                "the text is longer than a string may be ({LONGEST})"
            )));
        };
        m.build(text.chars_len())?;
        m.push(Value::Str(Rc::new(text)))
    }
    Print "print" (x: Any) "x --" "print x now, a line" {
        m.walk(&x);
        let text = x.text();
        host.print(&text).map_err(|f| m.fail(f.code, f.message))
    }
    Fail "fail" (x: Any) "x --" "stop the program, with x as the reason" {
        Err(m.fail(Code::Fail, x.text()))
    }
}
