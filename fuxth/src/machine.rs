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
        args: Vec<Value<Self::Value>>,
        out: &mut Vec<Value<Self::Value>>,
    ) -> Result<Done<Self::Request>, Failure>;
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
}

/// A program being run.
pub struct Machine<A> {
    program: Rc<Program<A>>,
    stack: Vec<Value<A>>,
    frames: Vec<Frame>,
    constants: Vec<Option<Value<A>>>,
    limits: Limits,
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
            }],
            constants,
            limits,
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
            // Past its last step, a definition returns.
            let Some(op) = code.and_then(|c| c.ops.get(frame.step)) else {
                self.frames.pop();
                continue;
            };
            frame.step = frame.step.saturating_add(1);
            match op {
                Op::Push(value) => self.push(value.clone())?,
                Op::Builtin(builtin) => self.builtin(*builtin)?,
                Op::App(id) => {
                    let takes = program.vocabulary.word(*id).map_or(0, |(_, takes)| takes);
                    let args = self.take(takes)?;
                    let mut out = Vec::new();
                    let done = host.call(*id, args, &mut out);
                    let done =
                        done.map_err(|f| self.fail(ErrorCode::Host(f.code), f.message, None))?;
                    for value in out {
                        self.push(value)?;
                    }
                    if let Done::Later(request) = done {
                        return Ok(Outcome::Suspended(request));
                    }
                }
                Op::Call(id) => {
                    if self.frames.len() >= self.limits.calls {
                        let message =
                            format!("calls nest past {}: recursion too deep", self.limits.calls);
                        return Err(self.fail(ErrorCode::CallDepth, message, None));
                    }
                    self.frames.push(Frame {
                        code: Some(*id),
                        step: 0,
                    });
                }
                Op::Jump(to) => self.jump(*to),
                Op::JumpUnless(to) => {
                    if !self.flag()? {
                        self.jump(*to);
                    }
                }
                Op::SetConst(slot) => {
                    let [value] = self.take_n::<1>()?;
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

    fn jump(&mut self, to: usize) {
        if let Some(frame) = self.frames.last_mut() {
            frame.step = to;
        }
    }

    /// Takes `n` values, the deepest first.
    fn take(&mut self, n: usize) -> Result<Vec<Value<A>>, Error> {
        let Some(from) = self.stack.len().checked_sub(n) else {
            let has = self.stack.len();
            let message = format!("takes {n} values, and the stack has {has}");
            return Err(self.fail(ErrorCode::StackUnderflow, message, None));
        };
        Ok(self.stack.split_off_checked(from))
    }

    fn take_n<const N: usize>(&mut self) -> Result<[Value<A>; N], Error> {
        let values = self.take(N)?;
        values
            .try_into()
            .map_err(|_| self.fail(ErrorCode::StackUnderflow, format!("takes {N} values"), None))
    }

    /// Takes a boolean, as `if` and `until` do.
    fn flag(&mut self) -> Result<bool, Error> {
        match self.take_n::<1>()? {
            [Value::Bool(flag)] => Ok(flag),
            [other] => {
                let message = format!(
                    "takes a boolean (from eq, lt, gt or not), not {}",
                    other.kind()
                );
                Err(self.fail(ErrorCode::Type, message, None))
            }
        }
    }

    fn ints(&mut self, builtin: Builtin) -> Result<(i64, i64), Error> {
        match self.take_n::<2>()? {
            [Value::Int(a), Value::Int(b)] => Ok((a, b)),
            [a, b] => {
                let message = format!(
                    "{} takes two integers, not {} and {}",
                    builtin.name(),
                    a.kind(),
                    b.kind()
                );
                Err(self.fail(ErrorCode::Type, message, None))
            }
        }
    }

    fn builtin(&mut self, builtin: Builtin) -> Result<(), Error> {
        let arithmetic = |a: i64, b: i64| -> Option<Result<i64, ErrorCode>> {
            let checked = match builtin {
                Builtin::Add => a.checked_add(b),
                Builtin::Sub => a.checked_sub(b),
                Builtin::Mul => a.checked_mul(b),
                Builtin::Div | Builtin::Mod if b == 0 => return Some(Err(ErrorCode::DivByZero)),
                Builtin::Div => a.checked_div(b),
                Builtin::Mod => a.checked_rem(b),
                Builtin::Dup
                | Builtin::Drop
                | Builtin::Swap
                | Builtin::Over
                | Builtin::Eq
                | Builtin::Lt
                | Builtin::Gt
                | Builtin::Not => return None,
            };
            Some(checked.ok_or(ErrorCode::Overflow))
        };
        match builtin {
            Builtin::Dup => {
                let [a] = self.take_n::<1>()?;
                self.push(a.clone())?;
                self.push(a)
            }
            Builtin::Drop => self.take_n::<1>().map(|_| ()),
            Builtin::Swap => {
                let [a, b] = self.take_n::<2>()?;
                self.push(b)?;
                self.push(a)
            }
            Builtin::Over => {
                let [a, b] = self.take_n::<2>()?;
                self.push(a.clone())?;
                self.push(b)?;
                self.push(a)
            }
            Builtin::Add | Builtin::Sub | Builtin::Mul | Builtin::Div | Builtin::Mod => {
                let (a, b) = self.ints(builtin)?;
                match arithmetic(a, b) {
                    Some(Ok(n)) => self.push(Value::Int(n)),
                    Some(Err(ErrorCode::DivByZero)) => {
                        Err(self.fail(ErrorCode::DivByZero, "division by zero".into(), None))
                    }
                    Some(Err(code)) => {
                        Err(self.fail(code, "past what an integer holds".into(), None))
                    }
                    None => Ok(()),
                }
            }
            Builtin::Eq => {
                let [a, b] = self.take_n::<2>()?;
                self.push(Value::Bool(a == b))
            }
            Builtin::Lt | Builtin::Gt => {
                let (a, b) = self.ints(builtin)?;
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

/// `split_off` without its panic: the values from `from` on.
trait SplitOffChecked<T> {
    fn split_off_checked(&mut self, from: usize) -> Vec<T>;
}

impl<T> SplitOffChecked<T> for Vec<T> {
    fn split_off_checked(&mut self, from: usize) -> Vec<T> {
        let keep = from.min(self.len());
        let mut all = std::mem::take(self).into_iter();
        self.extend(all.by_ref().take(keep));
        all.collect()
    }
}
