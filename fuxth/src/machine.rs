//! Running a compiled program. All its state is data: the stack, a frame
//! for each call (the code and the step it is at), the constants. So it can
//! stop anywhere and go on later: after a budget of steps, so its host can
//! look for Ctrl-C, and at an application word that needs its host to do
//! something first. It never runs as Rust recursion, so no program can
//! overflow the Rust stack, and stopping is just dropping it.
use crate::compile::{Builtin, Op, Program, Steps};
use crate::error::{Code as ErrorCode, Error, Failure};
use crate::token::Token;
use crate::value::{AppValue, Value};
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

/// A call under way: which code (`None`, the top level), and its next step.
#[derive(Clone, Copy, Debug)]
struct Frame {
    code: Option<usize>,
    step: usize,
    /// For a call in last place, which took its caller's frame: the call
    /// that began the chain of such calls, as a code and the step after it.
    /// Only the first is kept, so a loop written as recursion keeps one.
    via: Option<(Option<usize>, usize)>,
}

/// A program being run.
pub struct Machine<A> {
    program: Rc<Program<A>>,
    stack: Vec<Value<A>>,
    frames: Vec<Frame>,
    constants: Vec<Option<Value<A>>>,
    limits: Limits,
    /// What application words give, kept between calls.
    out: Vec<Value<A>>,
    /// A suspended application word: the stack's depth before it gave
    /// anything, and how many values it gives.
    pending: Option<(usize, RangeInclusive<usize>)>,
}

impl<A: AppValue> Machine<A> {
    pub fn new(program: Program<A>, limits: Limits) -> Machine<A> {
        let constants = vec![None; program.constants];
        Machine {
            program: Rc::new(program),
            stack: Vec::new(),
            frames: vec![Frame {
                code: None,
                step: 0,
                via: None,
            }],
            constants,
            limits,
            out: Vec::new(),
            pending: None,
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
            steps = steps.saturating_add(1);
            let Some(frame) = self.frames.last_mut() else {
                return Ok(Outcome::Finished);
            };
            let code = match frame.code {
                None => Some(&program.main),
                Some(i) => program.words.get(i),
            };
            let Some(code) = code else {
                self.frames.pop();
                continue;
            };
            // Past its last step, a definition returns.
            let Some(op) = code.ops.get(frame.step) else {
                self.frames.pop();
                continue;
            };
            frame.step = frame.step.saturating_add(1);
            match op {
                Op::Push(value) => self.push(value.clone())?,
                Op::Builtin(builtin) => self.builtin(*builtin)?,
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
                        self.frames.pop();
                    } else if self.frames.len() >= self.limits.calls {
                        let message =
                            format!("calls nest past {}: recursion too deep", self.limits.calls);
                        return Err(self.fail(ErrorCode::CallDepth, message, None));
                    }
                    self.frames.push(Frame {
                        code: Some(*id),
                        step: 0,
                        via,
                    });
                }
                Op::Jump(to) => frame.step = *to,
                Op::JumpUnless(to) => {
                    let to = *to;
                    if !self.flag()? {
                        self.jump(to);
                    }
                }
                Op::SetConst(slot) => {
                    let value = self.pop()?;
                    if let Some(constant) = self.constants.get_mut(*slot) {
                        *constant = Some(value);
                    }
                }
                Op::GetConst(slot) => match self.constants.get(*slot).cloned().flatten() {
                    Some(value) => self.push(value)?,
                    None => {
                        let message = "read before the line that sets it ran".to_owned();
                        return Err(self.fail(ErrorCode::UnsetConstant, message, None));
                    }
                },
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

    /// Where the top `n` values start, if the stack has them.
    fn depth(&self, n: usize) -> Result<usize, Error> {
        self.stack.len().checked_sub(n).ok_or_else(|| {
            let has = self.stack.len();
            let values = if n == 1 { "value" } else { "values" };
            let message = format!("takes {n} {values}, and the stack has {has}");
            self.fail(ErrorCode::StackUnderflow, message, None)
        })
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
            other @ (Value::Int(_) | Value::Str(_) | Value::App(_)) => {
                let message = format!(
                    "takes a boolean (from eq, lt, gt or not), not {}",
                    other.kind()
                );
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

    fn builtin(&mut self, builtin: Builtin) -> Result<(), Error> {
        match builtin {
            Builtin::Dup => {
                let a = self.pop()?;
                self.push(a.clone())?;
                self.push(a)
            }
            Builtin::Drop => self.pop().map(|_| ()),
            Builtin::Swap => {
                let (a, b) = self.pop2()?;
                self.push(b)?;
                self.push(a)
            }
            Builtin::Over => {
                let (a, b) = self.pop2()?;
                self.push(a.clone())?;
                self.push(b)?;
                self.push(a)
            }
            Builtin::Rot => {
                self.depth(3)?;
                let (b, c) = self.pop2()?;
                let a = self.pop()?;
                self.push(b)?;
                self.push(c)?;
                self.push(a)
            }
            Builtin::Add | Builtin::Sub | Builtin::Mul | Builtin::Div | Builtin::Mod => {
                let (a, b) = self.ints()?;
                let n = match builtin {
                    Builtin::Add => a.checked_add(b),
                    Builtin::Sub => a.checked_sub(b),
                    Builtin::Mul => a.checked_mul(b),
                    Builtin::Div | Builtin::Mod if b == 0 => {
                        return Err(self.fail(
                            ErrorCode::DivByZero,
                            "division by zero".into(),
                            None,
                        ));
                    }
                    Builtin::Div => a.checked_div(b),
                    // The remainder always fits: the smallest integer mod
                    // -1 is 0, though its quotient is too big.
                    Builtin::Mod => Some(a.checked_rem(b).unwrap_or(0)),
                    Builtin::Dup
                    | Builtin::Drop
                    | Builtin::Swap
                    | Builtin::Over
                    | Builtin::Rot
                    | Builtin::Eq
                    | Builtin::Lt
                    | Builtin::Gt
                    | Builtin::Not => None,
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
            Builtin::Eq => {
                let (a, b) = self.pop2()?;
                self.push(Value::Bool(a == b))
            }
            Builtin::Lt | Builtin::Gt => {
                let (a, b) = self.ints()?;
                let flag = if builtin == Builtin::Lt { a < b } else { a > b };
                self.push(Value::Bool(flag))
            }
            Builtin::Not => {
                let flag = self.flag()?;
                self.push(Value::Bool(!flag))
            }
        }
    }
}
