//! Checking a compiled program before it runs: how deep the stack is at
//! each step, and what kind of value each place on it holds, as far as can
//! be known. A step that would fail on every run reaching it, for too few
//! values or a value of the wrong kind, is an error before anything runs.
//! What can't be known (a loop that leaves values, a host word that gives
//! one only sometimes) is never an error: a program is refused only when it
//! surely fails.
//!
//! The stack is described from the top down: the kinds of the top values,
//! as far as they are known, and below them the rest, whose depth is a
//! range. In a definition the depth is counted from where the stack was
//! when it was called, which isn't known; each definition is summed up by
//! how many of its caller's values it surely uses and what it leaves, and a
//! call applies that summary.
use crate::compile::{Builtin, Op, Program, Steps};
use crate::error::{Code, Error};
use crate::value::Value;
use std::collections::BTreeSet;
use std::rc::Rc;

/// Past any real stack: an unbounded depth.
const BIG: i64 = 1 << 40;

/// How many top values have their kinds kept; deeper ones join the rest.
const KEPT: usize = 32;

/// How often a step may be reached by a new way before its depths are
/// taken as unbounded, so a loop's check ends.
const WIDEN_AFTER: u32 = 3;

/// How often a recursive definition is gone over before its summary is
/// taken as anything at all.
const RECURSION_ROUNDS: u32 = 8;

/// The kinds a value may be: a set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Kinds(u8);

const INT: Kinds = Kinds(1);
const BOOL: Kinds = Kinds(2);
const STR: Kinds = Kinds(4);
const APP: Kinds = Kinds(8);
const ANY: Kinds = Kinds(15);

impl Kinds {
    fn of<A>(value: &Value<A>) -> Kinds {
        match value {
            Value::Int(_) => INT,
            Value::Bool(_) => BOOL,
            Value::Str(_) => STR,
            Value::App(_) => APP,
        }
    }

    fn union(self, other: Kinds) -> Kinds {
        Kinds(self.0 | other.0)
    }

    fn may_be(self, kind: Kinds) -> bool {
        self.0 & kind.0 != 0
    }

    /// As an error says it: "an integer or a string".
    fn describe(self) -> String {
        let names = [
            (INT, "an integer"),
            (BOOL, "a boolean"),
            (STR, "a string"),
            (APP, "an application value"),
        ];
        let said: Vec<&str> = names
            .iter()
            .filter(|(kind, _)| self.may_be(*kind))
            .map(|(_, name)| *name)
            .collect();
        said.join(" or ")
    }
}

/// What the stack is known to be at a step.
#[derive(Clone, Debug, PartialEq, Eq)]
struct State {
    /// The top values' kinds, deepest first.
    top: Vec<Kinds>,
    /// How many values are below them, counted from the depth at entry:
    /// from `lo` to `hi`.
    lo: i64,
    hi: i64,
    /// How many of the values there at entry every way here has used.
    used: i64,
}

/// What a definition does, as its callers see it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Summary {
    /// How many of its caller's values every way through it uses, whether
    /// that way returns or not: fewer, and every call fails.
    need: i64,
    /// The stack where it returns, counted from where it was called;
    /// `None` if it never returns.
    exit: Option<State>,
}

impl Summary {
    /// What a definition might do, for one the checker can't follow.
    fn anything() -> Summary {
        let mut exit = State::start();
        exit.forget(BIG.saturating_neg(), BIG, Entry::Unknown);
        exit.used = 0;
        Summary {
            need: 0,
            exit: Some(exit),
        }
    }
}

/// A step that surely fails: why.
enum Failing {
    /// It takes `takes`, and the stack has from `lo` to `hi`.
    Underflow { takes: i64, lo: i64, hi: i64 },
    /// Its values can't be the kinds it takes.
    Type(String),
}

/// How deep the stack is where a unit's code starts: known for the top
/// level (empty), not for a definition.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Entry {
    Empty,
    Unknown,
}

impl State {
    fn start() -> State {
        State {
            top: Vec::new(),
            lo: 0,
            hi: 0,
            used: 0,
        }
    }

    /// The depth, from `lo` to `hi`, where it is known (the top level).
    fn depth(&self) -> (i64, i64) {
        let top = len(&self.top);
        (self.lo.saturating_add(top), self.hi.saturating_add(top))
    }

    /// Keeps the ranges true: no depth below empty where it starts empty,
    /// and every way here has used at least what the deepest it can be
    /// leaves.
    fn settle(&mut self, entry: Entry) {
        if entry == Entry::Empty {
            self.lo = self.lo.max(0);
        }
        self.used = self.used.max(self.hi.saturating_neg());
    }

    fn push(&mut self, kinds: Kinds) {
        if self.top.len() >= KEPT {
            self.top = self.top.iter().skip(1).copied().collect();
            self.lo = self.lo.saturating_add(1);
            self.hi = self.hi.saturating_add(1);
        }
        self.top.push(kinds);
    }

    /// Takes `n` values; their kinds, the deepest first.
    fn pop(&mut self, n: usize, entry: Entry) -> Result<Vec<Kinds>, Failing> {
        let from_rest = n.saturating_sub(self.top.len());
        let missing = i64::try_from(from_rest).unwrap_or(BIG);
        if entry == Entry::Empty && self.hi.saturating_sub(missing) < 0 {
            let (lo, hi) = self.depth();
            return Err(Failing::Underflow {
                takes: i64::try_from(n).unwrap_or(BIG),
                lo,
                hi,
            });
        }
        self.lo = self.lo.saturating_sub(missing);
        self.hi = self.hi.saturating_sub(missing);
        self.settle(entry);
        let keep = self.top.len().saturating_sub(n);
        let mut taken = vec![ANY; from_rest];
        taken.extend(self.top.iter().skip(keep).copied());
        self.top.truncate(keep);
        Ok(taken)
    }

    /// Forgets the top values' kinds, and adds from `lo` to `hi` values of
    /// any kind.
    fn forget(&mut self, lo: i64, hi: i64, entry: Entry) {
        let top = len(&self.top);
        self.top.clear();
        self.lo = self.lo.saturating_add(top).saturating_add(lo);
        self.hi = self.hi.saturating_add(top).saturating_add(hi);
        self.settle(entry);
    }

    /// What either way may be.
    fn join(&self, other: &State) -> State {
        let kept = self.top.len().min(other.top.len());
        let rest = |s: &State| len(&s.top).saturating_sub(i64::try_from(kept).unwrap_or(BIG));
        let top = self.top.iter().rev().zip(other.top.iter().rev());
        let mut top: Vec<Kinds> = top.take(kept).map(|(a, b)| a.union(*b)).collect();
        top.reverse();
        State {
            top,
            lo: (self.lo.saturating_add(rest(self))).min(other.lo.saturating_add(rest(other))),
            hi: (self.hi.saturating_add(rest(self))).max(other.hi.saturating_add(rest(other))),
            used: self.used.min(other.used),
        }
    }

    /// `joined` (this joined with a new way), with a depth that keeps
    /// changing taken as unbounded.
    fn widen(&self, mut joined: State, entry: Entry) -> State {
        let (lo, hi) = self.depth();
        let (new_lo, new_hi) = joined.depth();
        let top = len(&joined.top);
        if new_lo < lo {
            joined.lo = BIG.saturating_neg().saturating_sub(top);
        }
        if new_hi > hi {
            joined.hi = BIG;
        }
        joined.settle(entry);
        joined
    }
}

fn len(top: &[Kinds]) -> i64 {
    i64::try_from(top.len()).unwrap_or(BIG)
}

/// Checks `program`: the first step that surely fails, if one does.
pub(crate) fn check<A>(program: &Program<A>) -> Result<(), Error> {
    let mut summaries: Vec<Option<Summary>> = Vec::with_capacity(program.words.len());
    let mut first: Option<Error> = None;
    for (id, code) in program.words.iter().enumerate() {
        summaries.push(None);
        // Recursion: gone over until its summary stays the same, starting
        // from knowing nothing of what a call to itself does.
        for round in 0..=RECURSION_ROUNDS {
            let new = Unit::new(program, code, Entry::Unknown, &summaries).run();
            let current = summaries.get(id).cloned().flatten();
            let next = match (&current, new) {
                (_, None) => current.clone(),
                (None, Some(new)) => Some(new),
                (Some(_), Some(_)) if round >= RECURSION_ROUNDS => Some(Summary::anything()),
                (Some(old), Some(new)) => Some(Summary {
                    need: old.need.min(new.need),
                    exit: match (&old.exit, new.exit) {
                        (Some(old), Some(new)) => Some(old.widen(old.join(&new), Entry::Unknown)),
                        (Some(old), None) => Some(old.clone()),
                        (None, new) => new,
                    },
                }),
            };
            if next == current {
                break;
            }
            if let Some(slot) = summaries.get_mut(id) {
                *slot = next;
            }
        }
        let error = Unit::new(program, code, Entry::Unknown, &summaries).first_error();
        first = earliest(first, error);
    }
    let error = Unit::new(program, &program.main, Entry::Empty, &summaries).first_error();
    match earliest(first, error) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// The error written first in the source.
fn earliest(a: Option<Error>, b: Option<Error>) -> Option<Error> {
    use crate::token::Place;
    match (a, b) {
        (Some(a), Some(b)) => {
            let b_first = match (&a.place, &b.place) {
                (
                    Some(Place::Line { source: s, line: x }),
                    Some(Place::Line { source: t, line: y }),
                ) => s == t && y < x,
                (Some(Place::Argument(x)), Some(Place::Argument(y))) => y < x,
                (Some(_), Some(_) | None) | (None, Some(_) | None) => false,
            };
            Some(if b_first { b } else { a })
        }
        (a, None) => a,
        (None, b) => b,
    }
}

/// One code's check: the top level, or a definition.
struct Unit<'a, A> {
    program: &'a Program<A>,
    code: &'a Steps<A>,
    entry: Entry,
    summaries: &'a [Option<Summary>],
    /// The steps where a block starts: the first, every jump's target, and
    /// each step after a jump.
    starts: BTreeSet<usize>,
}

impl<'a, A> Unit<'a, A> {
    fn new(
        program: &'a Program<A>,
        code: &'a Steps<A>,
        entry: Entry,
        summaries: &'a [Option<Summary>],
    ) -> Unit<'a, A> {
        let mut starts = BTreeSet::from([0]);
        for (at, op) in code.ops.iter().enumerate() {
            if let Op::Jump(to) | Op::JumpUnless(to) = op {
                starts.insert(*to);
                starts.insert(at.saturating_add(1));
            }
        }
        Unit {
            program,
            code,
            entry,
            summaries,
            starts,
        }
    }

    /// The state at each block's start, once nothing changes. Only blocks'
    /// starts are kept, so a long program without jumps keeps one. `None`
    /// if it goes on past what any program needs, which only a mistake in
    /// the checker would make it do: then the code goes unchecked, and a
    /// program is never kept from running by it.
    ///
    /// With them, the least of what the ways that never return (a call to a
    /// definition that doesn't) had used of the entry's values.
    fn states(&self) -> Option<(std::collections::BTreeMap<usize, (State, u32)>, Option<i64>)> {
        let mut states = std::collections::BTreeMap::from([(0, (State::start(), 0))]);
        let mut todo = BTreeSet::from([0usize]);
        let mut diverged: Option<i64> = None;
        // A block's state changes a bounded number of times: widened twice,
        // its kept values shrinking, their kinds growing.
        let mut left = self.starts.len().saturating_add(1).saturating_mul(1024);
        while let Some(start) = todo.pop_first() {
            left = left.checked_sub(1)?;
            let Some((state, _)) = states.get(&start).cloned() else {
                continue;
            };
            for (to, next) in self.block(start, state, &mut |_, _| {}, &mut diverged) {
                let merged = match states.get(&to) {
                    None => Some((next, 0)),
                    Some((old, seen)) => {
                        let joined = old.join(&next);
                        let joined = if *seen >= WIDEN_AFTER {
                            old.widen(joined, self.entry)
                        } else {
                            joined
                        };
                        (joined != *old).then(|| (joined, seen.saturating_add(1)))
                    }
                };
                if let Some(merged) = merged {
                    states.insert(to, merged);
                    todo.insert(to);
                }
            }
        }
        Some((states, diverged))
    }

    /// What the code does, as a definition's callers see it: `None` if no
    /// way through it gets anywhere (each fails, or calls what is not known
    /// yet); anything at all, if the code goes unchecked.
    fn run(&self) -> Option<Summary> {
        let end = self.code.ops.len();
        let Some((mut states, diverged)) = self.states() else {
            return Some(Summary::anything());
        };
        let exit = states.remove(&end).map(|(state, _)| state);
        let need = match (&exit, diverged) {
            (Some(exit), Some(diverged)) => exit.used.min(diverged),
            (Some(exit), None) => exit.used,
            (None, Some(diverged)) => diverged,
            (None, None) => return None,
        };
        Some(Summary { need, exit })
    }

    fn first_error(&self) -> Option<Error> {
        let mut first: Option<(usize, Error)> = None;
        let (states, _) = self.states()?;
        for (start, (state, _)) in states {
            let mut failed = |at, error| {
                if first.as_ref().is_none_or(|(seen, _)| at < *seen) {
                    first = Some((at, error));
                }
            };
            self.block(start, state, &mut failed, &mut None);
        }
        first.map(|(_, error)| error)
    }

    /// Goes through the block at `start`, from `state`: the blocks it may
    /// go on to, and the state each starts with. A step that surely fails
    /// goes to `failed`, and the way through it ends there; a way that
    /// never returns ends too, the least it had used kept in `diverged`.
    fn block(
        &self,
        start: usize,
        mut state: State,
        failed: &mut dyn FnMut(usize, Error),
        diverged: &mut Option<i64>,
    ) -> Vec<(usize, State)> {
        let mut at = start;
        loop {
            let Some(op) = self.code.ops.get(at) else {
                return Vec::new();
            };
            let next = at.saturating_add(1);
            let step = self.step(op, &mut state);
            match step {
                Err(failing) => {
                    failed(at, self.error(at, op, failing));
                    return Vec::new();
                }
                Ok(false) => {
                    *diverged = Some(diverged.map_or(state.used, |d| d.min(state.used)));
                    return Vec::new();
                }
                Ok(true) => {}
            }
            match op {
                Op::Jump(to) => return vec![(*to, state)],
                Op::JumpUnless(to) => return vec![(next, state.clone()), (*to, state)],
                Op::Push(_)
                | Op::Builtin(_)
                | Op::App(_)
                | Op::Call(_)
                | Op::SetConst(_)
                | Op::GetConst(_) => {}
            }
            if self.starts.contains(&next) || next >= self.code.ops.len() {
                return vec![(next, state)];
            }
            at = next;
        }
    }

    /// One step: whether a run can go on past it.
    fn step(&self, op: &Op<A>, state: &mut State) -> Result<bool, Failing> {
        let entry = self.entry;
        match op {
            Op::Push(value) => state.push(Kinds::of(value)),
            Op::Builtin(builtin) => builtin_step(*builtin, state, entry)?,
            Op::App(id) => {
                let (takes, gives) = self
                    .program
                    .vocabulary
                    .word(*id)
                    .map_or((0, 0..=0), |(_, takes, gives)| (takes, gives));
                state.pop(takes, entry)?;
                let (lo, hi) = (*gives.start(), *gives.end());
                if lo == hi {
                    for _ in 0..lo {
                        state.push(ANY);
                    }
                } else {
                    let count = |n: usize| i64::try_from(n).unwrap_or(BIG);
                    state.forget(count(lo), count(hi), entry);
                }
            }
            Op::Call(id) => {
                let Some(Some(summary)) = self.summaries.get(*id) else {
                    // Nothing known of it yet: a call to itself, the first
                    // time through.
                    return Ok(false);
                };
                state.pop(usize::try_from(summary.need).unwrap_or(0), entry)?;
                let Some(exit) = &summary.exit else {
                    return Ok(false);
                };
                let lo = exit.lo.saturating_add(summary.need);
                let hi = exit.hi.saturating_add(summary.need);
                if lo != 0 || hi != 0 {
                    state.forget(lo, hi, entry);
                }
                for kinds in &exit.top {
                    state.push(*kinds);
                }
            }
            Op::Jump(_) => {}
            Op::JumpUnless(_) => flag(state, entry)?,
            Op::SetConst(_) => {
                state.pop(1, entry)?;
            }
            Op::GetConst(_) => state.push(ANY),
        }
        Ok(true)
    }

    fn error(&self, at: usize, op: &Op<A>, failing: Failing) -> Error {
        let token = self.code.tokens.get(at);
        let code = match failing {
            Failing::Underflow { .. } => Code::StackUnderflow,
            Failing::Type(_) => Code::Type,
        };
        let message = match failing {
            Failing::Underflow { takes, lo, hi } => {
                let values = if takes == 1 { "value" } else { "values" };
                let has = if lo == hi {
                    format!("{hi}")
                } else {
                    format!("at most {hi}")
                };
                let takes = match op {
                    Op::Call(_) => format!("needs at least {takes}"),
                    Op::Push(_)
                    | Op::Builtin(_)
                    | Op::App(_)
                    | Op::Jump(_)
                    | Op::JumpUnless(_)
                    | Op::SetConst(_)
                    | Op::GetConst(_) => format!("takes {takes}"),
                };
                format!("{takes} {values}, and the stack has {has} here")
            }
            Failing::Type(message) => message,
        };
        Error {
            code,
            message,
            place: token.map(|t| t.place.clone()),
            word: token.map(|t| Rc::clone(&t.text)),
        }
    }
}

/// Takes a boolean, as `if`, `until` and `not` do.
fn flag(state: &mut State, entry: Entry) -> Result<(), Failing> {
    let taken = state.pop(1, entry)?;
    match taken.as_slice() {
        [kinds] if !kinds.may_be(BOOL) => Err(Failing::Type(format!(
            "takes a boolean (from eq, lt, gt or not), not {}",
            kinds.describe()
        ))),
        [_] | [] | [_, _, ..] => Ok(()),
    }
}

fn builtin_step(builtin: Builtin, state: &mut State, entry: Entry) -> Result<(), Failing> {
    let taken = state.pop(builtin.takes(), entry)?;
    let ints = |taken: &[Kinds]| {
        let message = match taken {
            [a, b] if !a.may_be(INT) && !b.may_be(INT) => {
                format!("not {} and {}", a.describe(), b.describe())
            }
            [a, _] if !a.may_be(INT) => format!("and the one under the top is {}", a.describe()),
            [_, b] if !b.may_be(INT) => format!("and the top one is {}", b.describe()),
            [_, _] | [] | [_] | [_, _, _, ..] => return Ok(()),
        };
        Err(Failing::Type(format!("takes two integers, {message}")))
    };
    match (builtin, taken.as_slice()) {
        (Builtin::Dup, [a]) => {
            state.push(*a);
            state.push(*a);
        }
        (Builtin::Swap, [a, b]) => {
            state.push(*b);
            state.push(*a);
        }
        (Builtin::Over, [a, b]) => {
            state.push(*a);
            state.push(*b);
            state.push(*a);
        }
        (Builtin::Rot, [a, b, c]) => {
            state.push(*b);
            state.push(*c);
            state.push(*a);
        }
        (Builtin::Add | Builtin::Sub | Builtin::Mul | Builtin::Div | Builtin::Mod, taken) => {
            ints(taken)?;
            state.push(INT);
        }
        (Builtin::Lt | Builtin::Gt, taken) => {
            ints(taken)?;
            state.push(BOOL);
        }
        (Builtin::Eq, _) => state.push(BOOL),
        (Builtin::Not, [kinds]) if !kinds.may_be(BOOL) => {
            return Err(Failing::Type(format!(
                "takes a boolean (from eq, lt, gt or not), not {}",
                kinds.describe()
            )));
        }
        (Builtin::Not, _) => state.push(BOOL),
        (Builtin::Drop, _) | (Builtin::Dup | Builtin::Swap | Builtin::Over | Builtin::Rot, _) => {}
    }
    Ok(())
}
