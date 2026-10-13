//! Running a compiled program. All its state is data: the stack, a frame
//! for each call (the code and the step it is at), the `let` names, the
//! open `list` and `each` blocks. So it can stop anywhere and go on later:
//! after a budget of steps, so its host can look for Ctrl-C, and at an
//! application word that needs its host to do something first. It never
//! runs a program as Rust recursion, so no program can overflow the Rust
//! stack, and stopping is just dropping it.
use crate::compile::{Builtin, Op, Program, Steps};
use crate::error::{Code as ErrorCode, Error, Failure};
use crate::token::Token;
use crate::value::{AppValue, List, Value};
use std::cmp::Ordering;
use std::ops::RangeInclusive;
use std::rc::Rc;

/// What runs the application's words.
pub trait Host {
    type Value: AppValue;
    /// What the host is asked to do while the machine waits.
    type Request;
    /// Runs application word `word` (its id in the vocabulary) on `args`,
    /// as many values as it takes, the deepest first. What it gives goes in
    /// `out`, and it says whether it is done, or the machine is to wait
    /// while the host does `Request` (then the host pushes its results and
    /// runs the machine again).
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
/// nested `if`s).
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
    fn default() -> Limits {
        Limits {
            stack: 100_000,
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

/// A program being run.
pub struct Machine<A> {
    program: Rc<Program<A>>,
    stack: Vec<Value<A>>,
    frames: Vec<Frame>,
    /// Every frame's `let` names, each frame's from its `locals`.
    locals: Vec<Option<Value<A>>>,
    globals: Vec<Option<Value<A>>>,
    /// Where each open `list` began on the stack: nothing below may be
    /// taken inside it.
    marks: Vec<usize>,
    /// Each open `each`: what it walks, and the next element's index.
    iters: Vec<(Value<A>, usize)>,
    limits: Limits,
    /// What application words give, kept between calls.
    out: Vec<Value<A>>,
    /// A suspended application word: the stack's depth before it gave
    /// anything, and how many values it gives.
    pending: Option<(usize, RangeInclusive<usize>)>,
    /// Steps the last long step costs beyond its own.
    extra: u64,
    /// Elements built so far.
    built: u64,
}

impl<A: AppValue> Machine<A> {
    pub fn new(program: Program<A>, limits: Limits) -> Machine<A> {
        let globals = vec![None; program.globals];
        let locals = vec![None; program.main.locals];
        Machine {
            program: Rc::new(program),
            stack: Vec::new(),
            frames: vec![Frame {
                code: None,
                step: 0,
                via: None,
                locals: 0,
            }],
            locals,
            globals,
            marks: Vec::new(),
            iters: Vec::new(),
            limits,
            out: Vec::new(),
            pending: None,
            extra: 0,
            built: 0,
        }
    }

    /// The stack, deepest first.
    pub fn stack(&self) -> &[Value<A>] {
        &self.stack
    }

    /// Pushes a value: a suspended word's result.
    pub fn push(&mut self, value: Value<A>) -> Result<(), Error> {
        if self.stack.len() >= self.limits.stack {
            return Err(self.fail(ErrorCode::StackOverflow, "the stack is full".into(), None));
        }
        self.stack.push(value);
        Ok(())
    }

    /// The token the step just taken came from: where an error is.
    /// The word it ran last, and where: the one a suspension is for, which
    /// the host's failure in doing it may name.
    pub fn at(&self) -> Option<Token> {
        self.token()
    }

    /// The step each call under way is at, the outermost first: the word
    /// that called each definition, down to the word it ran last.
    /// A call in last place, which took its caller's frame, still shows
    /// the call that began it.
    pub fn calls(&self) -> Vec<Token> {
        let at = |code: Option<usize>, step: usize| {
            self.code(code)?.tokens.get(step.checked_sub(1)?).cloned()
        };
        (self.frames.iter())
            .flat_map(|frame| {
                let via = frame.via.and_then(|(code, step)| at(code, step));
                via.into_iter().chain(at(frame.code, frame.step))
            })
            .collect()
    }

    fn token(&self) -> Option<Token> {
        let frame = self.frames.last()?;
        let code = self.code(frame.code)?;
        code.tokens.get(frame.step.checked_sub(1)?).cloned()
    }

    fn code(&self, code: Option<usize>) -> Option<&Steps<A>> {
        match code {
            None => Some(&self.program.main),
            Some(i) => self.program.words.get(i),
        }
    }

    fn fail(&self, code: ErrorCode, message: String, at: Option<Token>) -> Error {
        let at = at.or_else(|| self.token());
        Error {
            code,
            message,
            place: at.as_ref().map(|t| t.place.clone()),
            word: at.map(|t| t.text),
        }
    }

    /// Ends the frame on top: its `let` names go with it.
    fn leave(&mut self) {
        if let Some(frame) = self.frames.pop() {
            self.locals.truncate(frame.locals);
        }
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
        let program = Rc::clone(&self.program);
        let mut steps = 0u64;
        loop {
            if steps >= budget {
                return Ok(Outcome::Paused);
            }
            steps = steps
                .saturating_add(1)
                .saturating_add(std::mem::take(&mut self.extra));
            let Some(frame) = self.frames.last_mut() else {
                return Ok(Outcome::Finished);
            };
            let code = match frame.code {
                None => Some(&program.main),
                Some(i) => program.words.get(i),
            };
            let Some(code) = code else {
                self.leave();
                continue;
            };
            // Past its last step, a definition returns.
            let Some(op) = code.ops.get(frame.step) else {
                self.leave();
                continue;
            };
            frame.step = frame.step.saturating_add(1);
            match op {
                Op::Push(value) => self.push(value.clone())?,
                Op::Builtin(builtin) => self.builtin(*builtin, host)?,
                Op::PickN(n) => self.pick(*n)?,
                Op::RollN(n) => self.roll(*n)?,
                Op::App(id) => {
                    let (takes, gives) = program
                        .vocabulary
                        .word(*id)
                        .map_or((0, 0..=0), |(_, takes, gives)| (takes, gives));
                    let from = self.depth(takes)?;
                    let mut out = std::mem::take(&mut self.out);
                    out.clear();
                    let args = Args {
                        stack: &mut self.stack,
                        from,
                        at: from,
                    };
                    let done = host.call(*id, args, &mut out);
                    let done =
                        done.map_err(|f| self.fail(ErrorCode::Host(f.code), f.message, None))?;
                    let given = (self.stack.len(), gives);
                    if self.stack.len().saturating_add(out.len()) > self.limits.stack {
                        return Err(self.fail(
                            ErrorCode::StackOverflow,
                            "the stack is full".into(),
                            None,
                        ));
                    }
                    self.stack.append(&mut out);
                    self.out = out;
                    match done {
                        Done::Now => self.given(given)?,
                        // What it gives comes with the host's answer.
                        Done::Later(request) => {
                            self.pending = Some(given);
                            return Ok(Outcome::Suspended(request));
                        }
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
                    } else if self.frames.len() >= self.limits.calls {
                        let message =
                            format!("calls nest past {}: recursion too deep", self.limits.calls);
                        return Err(self.fail(ErrorCode::CallDepth, message, None));
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
                    let to = *to;
                    if !self.flag()? {
                        self.jump(to);
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
                Op::Mark => self.marks.push(self.stack.len()),
                Op::Gather => {
                    let from = self.marks.pop().unwrap_or(0).min(self.stack.len());
                    let mut items = Vec::with_capacity(self.stack.len().saturating_sub(from));
                    while self.stack.len() > from {
                        items.extend(self.stack.pop());
                    }
                    items.reverse();
                    let list = self.list(items)?;
                    self.push(list)?;
                }
                Op::EachStart => {
                    let seq = self.pop()?;
                    match seq {
                        Value::Str(_) | Value::List(_) => self.iters.push((seq, 0)),
                        other @ (Value::Int(_) | Value::Bool(_) | Value::App(_)) => {
                            let message =
                                format!("each walks a string or a list, not {}", other.kind());
                            return Err(self.fail(ErrorCode::Type, message, None));
                        }
                    }
                }
                Op::EachNext(out) => {
                    let out = *out;
                    let next = match self.iters.last_mut() {
                        Some((seq, at)) => {
                            let item = element(seq, *at);
                            *at = at.saturating_add(1);
                            item
                        }
                        None => None,
                    };
                    match next {
                        Some(item) => self.push(item)?,
                        None => {
                            self.iters.pop();
                            self.jump(out);
                        }
                    }
                }
            }
        }
    }

    /// A `let` name's value pushed; `unset` if its `let` hasn't run.
    fn named(&mut self, value: Option<Value<A>>) -> Result<(), Error> {
        match value {
            Some(value) => self.push(value),
            None => {
                let message = "read before its let ran".to_owned();
                Err(self.fail(ErrorCode::Unset, message, None))
            }
        }
    }

    /// Whether an application word gave as many values as its vocabulary
    /// says: the checker counted on it. `depth` is the stack's before it
    /// gave any.
    fn given(&self, (depth, gives): (usize, RangeInclusive<usize>)) -> Result<(), Error> {
        let given = self.stack.len().saturating_sub(depth);
        if gives.contains(&given) {
            return Ok(());
        }
        let message = format!(
            "the application gave {given} values, and its word says {}..={}",
            gives.start(),
            gives.end()
        );
        Err(self.fail(ErrorCode::Contract, message, None))
    }

    fn jump(&mut self, to: usize) {
        if let Some(frame) = self.frames.last_mut() {
            frame.step = to;
        }
    }

    /// Where the top `n` values start, if the stack has them: inside a
    /// `list`, only those above where it began.
    fn depth(&self, n: usize) -> Result<usize, Error> {
        let floor = self.marks.last().copied().unwrap_or(0);
        let has = self.stack.len().saturating_sub(floor);
        if has >= n {
            return Ok(self.stack.len().saturating_sub(n));
        }
        let values = if n == 1 { "value" } else { "values" };
        let inside = if self.marks.is_empty() {
            ""
        } else {
            " (inside list … end, which starts with an empty stack)"
        };
        let message = format!("takes {n} {values}, and the stack has {has}{inside}");
        Err(self.fail(ErrorCode::StackUnderflow, message, None))
    }

    fn pop(&mut self) -> Result<Value<A>, Error> {
        self.depth(1)?;
        self.stack
            .pop()
            .ok_or_else(|| self.fail(ErrorCode::StackUnderflow, "takes 1 value".into(), None))
    }

    /// The top two values, the deeper first.
    fn pop2(&mut self) -> Result<(Value<A>, Value<A>), Error> {
        self.depth(2)?;
        let b = self.pop()?;
        let a = self.pop()?;
        Ok((a, b))
    }

    /// Takes a boolean, as `if` and `until` do.
    fn flag(&mut self) -> Result<bool, Error> {
        match self.pop()? {
            Value::Bool(flag) => Ok(flag),
            other @ (Value::Int(_) | Value::Str(_) | Value::List(_) | Value::App(_)) => {
                let message = format!("takes a boolean (from eq or lt), not {}", other.kind());
                Err(self.fail(ErrorCode::Type, message, None))
            }
        }
    }

    fn ints(&mut self) -> Result<(i64, i64), Error> {
        match self.pop2()? {
            (Value::Int(a), Value::Int(b)) => Ok((a, b)),
            (a, b) => {
                let message = format!("takes two integers, not {} and {}", a.kind(), b.kind());
                Err(self.fail(ErrorCode::Type, message, None))
            }
        }
    }

    fn type_error(&self, message: String) -> Error {
        self.fail(ErrorCode::Type, message, None)
    }

    fn range_error(&self, message: String) -> Error {
        self.fail(ErrorCode::Range, message, None)
    }

    fn too_long(&self, message: String) -> Error {
        self.fail(ErrorCode::TooLong, message, None)
    }

    /// Takes a count: an integer, 0 or more.
    fn count(&mut self, what: &str) -> Result<usize, Error> {
        match self.pop()? {
            Value::Int(n) => usize::try_from(n)
                .map_err(|_| self.range_error(format!("{what} takes a count from 0, not {n}"))),
            other @ (Value::Bool(_) | Value::Str(_) | Value::List(_) | Value::App(_)) => {
                Err(self.type_error(format!("{what} takes an integer, not {}", other.kind())))
            }
        }
    }

    /// Charges a long step for going through `n` elements.
    fn cost(&mut self, n: usize) {
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        self.extra = self.extra.saturating_add(n / PER_STEP);
    }

    /// Counts `n` elements built: past what a run may build in all, or past
    /// what one value may hold, a `too_long` error.
    fn build(&mut self, n: usize) -> Result<(), Error> {
        if n > LONGEST {
            return Err(self.too_long(format!(
                "{n} elements, and a string or list holds at most {LONGEST}"
            )));
        }
        self.cost(n);
        self.built = self
            .built
            .saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
        if self.built > BUILT {
            return Err(self.too_long(format!("built more than {BUILT} elements in all")));
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
        self.too_long(format!("lists nest at most {} deep", crate::value::NESTING))
    }

    fn pick(&mut self, n: usize) -> Result<(), Error> {
        let from = self.depth(n.saturating_add(1))?;
        match self.stack.get(from).cloned() {
            Some(value) => self.push(value),
            None => Err(self.fail(
                ErrorCode::StackUnderflow,
                "pick reached past the stack".into(),
                None,
            )),
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

    fn builtin<H: Host<Value = A>>(&mut self, builtin: Builtin, host: &mut H) -> Result<(), Error> {
        match builtin {
            Builtin::Drop => self.pop().map(|_| ()),
            Builtin::Pick => {
                let n = self.count("pick")?;
                self.pick(n)
            }
            Builtin::Roll => {
                let n = self.count("roll")?;
                self.roll(n)
            }
            Builtin::Add | Builtin::Sub | Builtin::Mul | Builtin::Div => {
                let (a, b) = self.ints()?;
                if builtin == Builtin::Div && b == 0 {
                    return Err(self.fail(ErrorCode::DivByZero, "division by zero".into(), None));
                }
                let n = match builtin {
                    Builtin::Add => a.checked_add(b),
                    Builtin::Sub => a.checked_sub(b),
                    Builtin::Mul => a.checked_mul(b),
                    Builtin::Div => a.checked_div(b),
                    Builtin::Drop
                    | Builtin::Pick
                    | Builtin::Roll
                    | Builtin::Lt
                    | Builtin::Eq
                    | Builtin::Len
                    | Builtin::At
                    | Builtin::Slice
                    | Builtin::Concat
                    | Builtin::Push
                    | Builtin::Put
                    | Builtin::Code
                    | Builtin::Char
                    | Builtin::Kind
                    | Builtin::String
                    | Builtin::Print
                    | Builtin::Fail => None,
                };
                match n {
                    Some(n) => self.push(Value::Int(n)),
                    None => Err(self.fail(
                        ErrorCode::Overflow,
                        "past what an integer holds".into(),
                        None,
                    )),
                }
            }
            Builtin::Lt => {
                let (a, b) = self.pop2()?;
                self.cost(a.size().min(b.size()));
                match compare(&a, &b) {
                    Some(order) => self.push(Value::Bool(order == Ordering::Less)),
                    None => Err(self.type_error(format!(
                        "lt compares two integers, two strings or two lists, not {} and {}",
                        a.kind(),
                        b.kind()
                    ))),
                }
            }
            Builtin::Eq => {
                let (a, b) = self.pop2()?;
                self.cost(a.size().min(b.size()));
                self.push(Value::Bool(a == b))
            }
            Builtin::Len => {
                let seq = self.pop()?;
                let n = self.seq_len(&seq, "len")?;
                if matches!(seq, Value::Str(_)) {
                    self.cost(n);
                }
                self.push(Value::Int(i64::try_from(n).unwrap_or(i64::MAX)))
            }
            Builtin::At => {
                let i = self.pop()?;
                let seq = self.pop()?;
                let len = self.seq_len(&seq, "at")?;
                let i = self.index(&i, len, "at")?;
                if matches!(seq, Value::Str(_)) {
                    self.cost(i);
                }
                match element(&seq, i) {
                    Some(item) => self.push(item),
                    None => Err(self.range_error(format!("index {i}, and it has {len}"))),
                }
            }
            Builtin::Slice => {
                let to = self.pop()?;
                let from = self.pop()?;
                let seq = self.pop()?;
                let len = self.seq_len(&seq, "slice")?;
                let (Value::Int(a), Value::Int(b)) = (&from, &to) else {
                    return Err(self.type_error(format!(
                        "slice takes two integers after the sequence, not {} and {}",
                        from.kind(),
                        to.kind()
                    )));
                };
                let (a, b) = (*a, *b);
                let bounds = usize::try_from(a).ok().zip(usize::try_from(b).ok());
                let Some((from, to)) = bounds.filter(|(f, t)| f <= t && *t <= len) else {
                    return Err(
                        self.range_error(format!("slice from {a} to {b}, and it has {len}"))
                    );
                };
                let n = to.saturating_sub(from);
                let part = match &seq {
                    Value::Str(s) => {
                        self.build(n)?;
                        Value::str(s.chars().skip(from).take(n).collect::<String>())
                    }
                    Value::List(list) => {
                        let items = list.items.iter().skip(from).take(n).cloned().collect();
                        self.list(items)?
                    }
                    Value::Int(_) | Value::Bool(_) | Value::App(_) => seq.clone(),
                };
                self.push(part)
            }
            Builtin::Concat => {
                let list = self.pop()?;
                let Value::List(list) = list else {
                    return Err(self.type_error(format!(
                        "concat takes a list of strings or of lists, not {}",
                        list.kind()
                    )));
                };
                let joined = self.concat(&list)?;
                self.push(joined)
            }
            Builtin::Push => {
                let x = self.pop()?;
                let seq = self.pop()?;
                let pushed = self.pushed(seq, x)?;
                self.push(pushed)
            }
            Builtin::Put => {
                let x = self.pop()?;
                let i = self.pop()?;
                let seq = self.pop()?;
                let len = self.seq_len(&seq, "put")?;
                let i = self.index(&i, len, "put")?;
                let put = self.put(seq, i, x)?;
                self.push(put)
            }
            Builtin::Code => {
                let c = self.pop()?;
                let one = match &c {
                    Value::Str(s) => {
                        let mut chars = s.chars();
                        chars.next().filter(|_| chars.next().is_none())
                    }
                    Value::Int(_) | Value::Bool(_) | Value::List(_) | Value::App(_) => None,
                };
                match one {
                    Some(ch) => self.push(Value::Int(i64::from(u32::from(ch)))),
                    None => Err(self.type_error(format!(
                        "code takes a one-character string, not {}",
                        match &c {
                            Value::Str(s) if s.is_empty() => "an empty one",
                            Value::Str(_) => "a longer one",
                            Value::Int(_) | Value::Bool(_) | Value::List(_) | Value::App(_) => {
                                c.kind()
                            }
                        }
                    ))),
                }
            }
            Builtin::Char => {
                let n = self.pop()?;
                let Value::Int(n) = n else {
                    return Err(self.type_error(format!("char takes an integer, not {}", n.kind())));
                };
                match u32::try_from(n).ok().and_then(char::from_u32) {
                    Some(ch) => self.push(Value::str(ch.to_string())),
                    None => Err(self.range_error(format!("{n} is no character code"))),
                }
            }
            Builtin::Kind => {
                let x = self.pop()?;
                self.push(Value::str(x.kind_name()))
            }
            Builtin::String => {
                let x = self.pop()?;
                let text = x.text();
                self.build(text.len())?;
                self.push(Value::str(text))
            }
            Builtin::Print => {
                let x = self.pop()?;
                let text = x.text();
                self.cost(text.len());
                host.print(&text)
                    .map_err(|f| self.fail(ErrorCode::Host(f.code), f.message, None))
            }
            Builtin::Fail => {
                let x = self.pop()?;
                Err(self.fail(ErrorCode::Fail, x.text(), None))
            }
        }
    }

    /// How many elements `seq` has, if it is a string or list.
    fn seq_len(&self, seq: &Value<A>, word: &str) -> Result<usize, Error> {
        match seq {
            Value::Str(_) | Value::List(_) => Ok(seq.size()),
            Value::Int(_) | Value::Bool(_) | Value::App(_) => Err(self.type_error(format!(
                "{word} takes a string or a list, not {}",
                seq.kind()
            ))),
        }
    }

    /// An index into something `len` long.
    fn index(&self, i: &Value<A>, len: usize, word: &str) -> Result<usize, Error> {
        match i {
            Value::Int(n) => usize::try_from(*n)
                .ok()
                .filter(|i| *i < len)
                .ok_or_else(|| self.range_error(format!("index {n}, and it has {len}"))),
            Value::Bool(_) | Value::Str(_) | Value::List(_) | Value::App(_) => {
                Err(self.type_error(format!("{word} takes an integer index, not {}", i.kind())))
            }
        }
    }

    fn concat(&mut self, list: &List<A>) -> Result<Value<A>, Error> {
        let mut text = String::new();
        let mut items = Vec::new();
        let (mut strings, mut lists) = (false, false);
        for item in &list.items {
            match item {
                Value::Str(s) => {
                    strings = true;
                    text.push_str(s);
                }
                Value::List(l) => {
                    lists = true;
                    items.extend(l.items.iter().cloned());
                }
                Value::Int(_) | Value::Bool(_) | Value::App(_) => {
                    return Err(self.type_error(format!(
                        "concat takes a list of strings or of lists, and this one holds {}",
                        item.kind()
                    )));
                }
            }
            if strings && lists {
                return Err(
                    self.type_error("concat takes a list of strings or of lists, not both".into())
                );
            }
        }
        if strings {
            self.build(text.chars().count())?;
            Ok(Value::str(text))
        } else {
            self.list(items)
        }
    }

    fn pushed(&mut self, seq: Value<A>, x: Value<A>) -> Result<Value<A>, Error> {
        match (seq, x) {
            (Value::List(mut list), x) => {
                if list.items.len() >= LONGEST {
                    return Err(self.too_long(format!("a list holds at most {LONGEST} elements")));
                }
                self.build(1)?;
                // In place when nothing else holds the list.
                match Rc::make_mut(&mut list).push(x) {
                    Some(()) => Ok(Value::List(list)),
                    None => Err(self.nested()),
                }
            }
            (Value::Str(s), Value::Str(x)) => {
                let n = s.chars().count().saturating_add(x.chars().count());
                self.build(n)?;
                let mut text = String::with_capacity(s.len().saturating_add(x.len()));
                text.push_str(&s);
                text.push_str(&x);
                Ok(Value::str(text))
            }
            (Value::Str(_), x) => Err(self.type_error(format!(
                "push onto a string takes a string, not {}",
                x.kind()
            ))),
            (seq @ (Value::Int(_) | Value::Bool(_) | Value::App(_)), _) => {
                Err(self.type_error(format!("push takes a string or a list, not {}", seq.kind())))
            }
        }
    }

    fn put(&mut self, seq: Value<A>, i: usize, x: Value<A>) -> Result<Value<A>, Error> {
        match (seq, x) {
            (Value::List(mut list), x) => match Rc::make_mut(&mut list).put(i, x) {
                Some(()) => Ok(Value::List(list)),
                None => Err(self.nested()),
            },
            (Value::Str(s), Value::Str(x)) => {
                self.build(s.chars().count().saturating_add(x.chars().count()))?;
                let mut text = String::with_capacity(s.len().saturating_add(x.len()));
                for (at, c) in s.chars().enumerate() {
                    if at == i {
                        text.push_str(&x);
                    } else {
                        text.push(c);
                    }
                }
                Ok(Value::str(text))
            }
            (Value::Str(_), x) => Err(self.type_error(format!(
                "put into a string takes a string, not {}",
                x.kind()
            ))),
            (seq @ (Value::Int(_) | Value::Bool(_) | Value::App(_)), _) => {
                Err(self.type_error(format!("put takes a string or a list, not {}", seq.kind())))
            }
        }
    }
}

/// Element `i` of a string (a one-character string) or a list.
fn element<A: Clone>(seq: &Value<A>, i: usize) -> Option<Value<A>> {
    match seq {
        Value::Str(s) => s.chars().nth(i).map(|c| Value::str(c.to_string())),
        Value::List(list) => list.items.get(i).cloned(),
        Value::Int(_) | Value::Bool(_) | Value::App(_) => None,
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
            for (x, y) in a.items.iter().zip(&b.items) {
                if x != y {
                    return compare(x, y);
                }
            }
            Some(a.items.len().cmp(&b.items.len()))
        }
        (Value::Int(_) | Value::Bool(_) | Value::Str(_) | Value::List(_) | Value::App(_), _) => {
            None
        }
    }
}
