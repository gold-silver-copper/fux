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
//! range. At the top level, and inside `list … end`, the depth is counted
//! from an empty stack. In a definition it is counted from where the stack
//! was when it was called, which isn't known; each definition is summed up
//! by how many of its caller's values it surely uses and what it leaves,
//! and a call applies that summary.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Kinds(u8);

const INT: Kinds = Kinds(1);
const BOOL: Kinds = Kinds(2);
const STR: Kinds = Kinds(4);
const LIST: Kinds = Kinds(8);
const APP: Kinds = Kinds(16);
const ANY: Kinds = Kinds(31);
/// A string or a list.
const SEQ: Kinds = Kinds(12);

impl Kinds {
    fn of<A>(value: &Value<A>) -> Kinds {
        match value {
            Value::Int(_) => INT,
            Value::Bool(_) => BOOL,
            Value::Str(_) => STR,
            Value::List(_) => LIST,
            Value::App(_) => APP,
        }
    }

    fn union(self, other: Kinds) -> Kinds {
        Kinds(self.0 | other.0)
    }

    fn and(self, other: Kinds) -> Kinds {
        Kinds(self.0 & other.0)
    }

    fn may_be(self, kind: Kinds) -> bool {
        self.0 & kind.0 != 0
    }

    /// As an error says it: "an integer or a string".
    fn describe(self) -> String {
        if self == ANY {
            return "any value".to_owned();
        }
        let names = [
            (INT, "an integer"),
            (BOOL, "a boolean"),
            (STR, "a string"),
            (LIST, "a list"),
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
    /// The deepest any way here has reached below the depth at entry: the
    /// values down to there may have been replaced.
    reached: i64,
    /// Whether the depth is counted from an empty stack (the top level, or
    /// a `list` body), so too few values is an error here.
    bounded: bool,
    /// The states each open `list` began from, innermost last.
    outer: Vec<State>,
    /// What kind each open `each` gives, innermost last.
    iters: Vec<Kinds>,
    /// Whether a `roll` of a depth not known before running ran: values
    /// below may have moved, so what was known of their kinds is not.
    scrambled: bool,
    /// The kinds the code's own `let` names hold, by slot.
    names: Vec<Kinds>,
    /// The kinds the top level's `let` names hold, by slot: kept only at
    /// the top level (a definition may run when one has changed).
    globals: Vec<Kinds>,
}

/// The kinds slot `slot` of `names` holds: any, if not known.
fn named(names: &[Kinds], slot: usize) -> Kinds {
    names.get(slot).copied().unwrap_or(ANY)
}

/// Sets slot `slot` of `names` to `kinds`.
fn name(names: &mut Vec<Kinds>, slot: usize, kinds: Kinds) {
    if names.len() <= slot {
        names.resize(slot.saturating_add(1), ANY);
    }
    if let Some(at) = names.get_mut(slot) {
        *at = kinds;
    }
}

/// Either way's kinds, slot by slot.
fn join_names(a: &[Kinds], b: &[Kinds]) -> Vec<Kinds> {
    let n = a.len().max(b.len());
    (0..n).map(|i| named(a, i).union(named(b, i))).collect()
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
    /// Whether it may move its caller's values (a `roll` of unknown depth).
    scrambled: bool,
}

/// How many of a definition's inputs a call's known kinds are given to it
/// with, to check it again for that call.
const GIVEN: usize = 8;

/// A definition checked again with the kinds a call gives it.
#[derive(Clone, Debug)]
enum Special {
    /// What it does given them.
    Does(Summary),
    /// Every way through it fails given them: why.
    Fails(String),
    /// Nothing more is known than its summary says.
    Unknown,
}

/// The definitions checked again, by definition and the kinds given.
type Specials = std::cell::RefCell<std::collections::HashMap<(usize, Vec<Kinds>), Special>>;

impl Summary {
    /// What a definition might do, for one the checker can't follow.
    fn anything() -> Summary {
        let mut exit = State::start(false);
        exit.forget(BIG.saturating_neg(), BIG);
        exit.used = 0;
        Summary {
            need: 0,
            exit: Some(exit),
            scrambled: true,
        }
    }
}

/// A step that surely fails: why.
enum Failing {
    /// It takes `takes`, and the stack has from `lo` to `hi`; `inside`
    /// a `list` body.
    Underflow {
        takes: u64,
        lo: i64,
        hi: i64,
        inside: bool,
    },
    /// Its values can't be the kinds it takes.
    Type(String),
}

/// Whether a run can go on past a step.
enum Flow {
    Go,
    /// It never returns, and doesn't fail: a call to a definition that
    /// doesn't return.
    Diverge,
    /// It stops the run on purpose (`fail`).
    Stop,
}

impl State {
    fn start(bounded: bool) -> State {
        State {
            top: Vec::new(),
            lo: 0,
            hi: 0,
            used: 0,
            reached: 0,
            bounded,
            outer: Vec::new(),
            iters: Vec::new(),
            scrambled: false,
            names: Vec::new(),
            globals: Vec::new(),
        }
    }

    /// The depth, from `lo` to `hi`, where it is known.
    fn depth(&self) -> (i64, i64) {
        let top = len(&self.top);
        (self.lo.saturating_add(top), self.hi.saturating_add(top))
    }

    /// Keeps the ranges true: no depth below empty where it starts empty,
    /// and every way here has used at least what the deepest it can be
    /// leaves.
    fn settle(&mut self) {
        if self.bounded {
            self.lo = self.lo.max(0);
        }
        self.used = self.used.max(self.hi.saturating_neg());
        self.reached = self.reached.min(self.lo);
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
    fn pop(&mut self, n: usize) -> Result<Vec<Kinds>, Failing> {
        let from_rest = n.saturating_sub(self.top.len());
        let missing = i64::try_from(from_rest).unwrap_or(BIG);
        if self.bounded && self.hi.saturating_sub(missing) < 0 {
            let (lo, hi) = self.depth();
            return Err(Failing::Underflow {
                takes: u64::try_from(n).unwrap_or(u64::MAX),
                lo,
                hi,
                inside: !self.outer.is_empty(),
            });
        }
        self.lo = self.lo.saturating_sub(missing);
        self.hi = self.hi.saturating_sub(missing);
        self.settle();
        let keep = self.top.len().saturating_sub(n);
        let mut taken = vec![ANY; from_rest];
        taken.extend(self.top.iter().skip(keep).copied());
        self.top.truncate(keep);
        Ok(taken)
    }

    /// Forgets the top values' kinds, and adds from `lo` to `hi` values of
    /// any kind.
    fn forget(&mut self, lo: i64, hi: i64) {
        let top = len(&self.top);
        self.top.clear();
        self.lo = self.lo.saturating_add(top).saturating_add(lo);
        self.hi = self.hi.saturating_add(top).saturating_add(hi);
        self.settle();
    }

    /// What either way may be.
    fn join(&self, other: &State) -> State {
        let kept = self.top.len().min(other.top.len());
        let rest = |s: &State| len(&s.top).saturating_sub(i64::try_from(kept).unwrap_or(BIG));
        let top = self.top.iter().rev().zip(other.top.iter().rev());
        let mut top: Vec<Kinds> = top.take(kept).map(|(a, b)| a.union(*b)).collect();
        top.reverse();
        let outer = if self.outer.len() == other.outer.len() {
            (self.outer.iter().zip(&other.outer))
                .map(|(a, b)| a.join(b))
                .collect()
        } else {
            self.outer.clone()
        };
        let iters = if self.iters.len() == other.iters.len() {
            (self.iters.iter().zip(&other.iters))
                .map(|(a, b)| a.union(*b))
                .collect()
        } else {
            self.iters.clone()
        };
        State {
            top,
            lo: (self.lo.saturating_add(rest(self))).min(other.lo.saturating_add(rest(other))),
            hi: (self.hi.saturating_add(rest(self))).max(other.hi.saturating_add(rest(other))),
            used: self.used.min(other.used),
            reached: self.reached.min(other.reached),
            bounded: self.bounded,
            outer,
            iters,
            scrambled: self.scrambled || other.scrambled,
            names: join_names(&self.names, &other.names),
            globals: join_names(&self.globals, &other.globals),
        }
    }

    /// `joined` (this joined with a new way), with a depth that keeps
    /// changing taken as unbounded.
    fn widen(&self, mut joined: State) -> State {
        let (lo, hi) = self.depth();
        let (new_lo, new_hi) = joined.depth();
        let top = len(&joined.top);
        if new_lo < lo {
            joined.lo = BIG.saturating_neg().saturating_sub(top);
        }
        if new_hi > hi {
            joined.hi = BIG;
        }
        joined.settle();
        joined
    }
}

fn len(top: &[Kinds]) -> i64 {
    i64::try_from(top.len()).unwrap_or(BIG)
}

/// Checks `program`: the first step that surely fails, if one does.
pub(crate) fn check<A>(program: &Program<A>, mut known: Known) -> Result<(), Error> {
    definitions(program, &mut known);
    let Known {
        summaries,
        first,
        specials,
    } = known;
    let main = State::start(true);
    let error = Unit::new(program, &program.main, main, &summaries, &specials, None).first_error();
    match earliest(first, error) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// What the checker knows of a program's definitions so far: what each
/// does, and the first mistake among them. A library's, worked out once,
/// serves every program compiled after it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Known {
    summaries: Vec<Option<Summary>>,
    first: Option<Error>,
    specials: Specials,
}

/// Works out what each of `program`'s definitions does, from the first
/// `known` doesn't know yet.
pub(crate) fn definitions<A>(program: &Program<A>, known: &mut Known) {
    let Known {
        summaries,
        first,
        specials,
    } = known;
    let from = summaries.len();
    for (id, code) in program.words.iter().enumerate().skip(from) {
        summaries.push(None);
        // Recursion: gone over until its summary stays the same, starting
        // from knowing nothing of what a call to itself does.
        for round in 0..=RECURSION_ROUNDS {
            let unit = Unit::new(
                program,
                code,
                State::start(false),
                summaries,
                specials,
                Some(id),
            );
            let new = unit.run();
            let current = summaries.get(id).cloned().flatten();
            let next = match (&current, new) {
                (_, None) => current.clone(),
                (None, Some(new)) => Some(new),
                (Some(_), Some(_)) if round >= RECURSION_ROUNDS => Some(Summary::anything()),
                (Some(old), Some(new)) => Some(Summary {
                    need: old.need.min(new.need),
                    exit: match (&old.exit, new.exit) {
                        (Some(old), Some(new)) => Some(old.widen(old.join(&new))),
                        (Some(old), None) => Some(old.clone()),
                        (None, new) => new,
                    },
                    scrambled: old.scrambled || new.scrambled,
                }),
            };
            if next == current {
                break;
            }
            if let Some(slot) = summaries.get_mut(id) {
                *slot = next;
            }
        }
        let unit = Unit::new(
            program,
            code,
            State::start(false),
            summaries,
            specials,
            Some(id),
        );
        let error = unit.first_error();
        *first = earliest(first.take(), error);
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

/// Each block's state, by the step it starts at, with how often it has
/// changed.
type Blocks = std::collections::BTreeMap<usize, (State, u32)>;

/// One code's check: the top level, or a definition.
struct Unit<'a, A> {
    program: &'a Program<A>,
    code: &'a Steps<A>,
    /// The stack where the code starts: empty at the top level.
    start: State,
    /// Whether it is the top level, whose `let` names it follows.
    top: bool,
    summaries: &'a [Option<Summary>],
    specials: &'a Specials,
    /// The definition it is, if one: a call to itself uses its summary.
    own: Option<usize>,
    /// The steps where a block starts: the first, every jump's target, and
    /// each step after a jump.
    starts: BTreeSet<usize>,
}

impl<'a, A> Unit<'a, A> {
    fn new(
        program: &'a Program<A>,
        code: &'a Steps<A>,
        start: State,
        summaries: &'a [Option<Summary>],
        specials: &'a Specials,
        own: Option<usize>,
    ) -> Unit<'a, A> {
        let mut starts = BTreeSet::from([0]);
        for (at, op) in code.ops.iter().enumerate() {
            if let Op::Jump(to) | Op::JumpUnless(to) | Op::EachNext(to) = op {
                starts.insert(*to);
                starts.insert(at.saturating_add(1));
            }
        }
        let top = std::ptr::eq(code, &program.main);
        Unit {
            program,
            code,
            start,
            top,
            summaries,
            specials,
            own,
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
    ///
    /// `forward`: only the way through once, no loop gone round again (a
    /// way that would go round ends there, its use kept as if it ended):
    /// every run goes that far.
    fn states(&self, forward: bool) -> Option<(Blocks, Option<i64>)> {
        let first = (self.start.clone(), 0);
        let mut states = std::collections::BTreeMap::from([(0, first)]);
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
                if forward && to <= start {
                    let used = next.outer.first().map_or(next.used, |o| o.used);
                    diverged = Some(diverged.map_or(used, |d| d.min(used)));
                    continue;
                }
                let merged = match states.get(&to) {
                    None => Some((next, 0)),
                    Some((old, seen)) => {
                        let joined = old.join(&next);
                        let joined = if *seen >= WIDEN_AFTER {
                            old.widen(joined)
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
        let Some((mut states, diverged)) = self.states(false) else {
            return Some(Summary::anything());
        };
        let scrambled = states.values().any(|(s, _)| s.scrambled);
        let exit = states.remove(&end).map(|(state, _)| state);
        let need = |exit: Option<&State>, diverged: Option<i64>| match (exit, diverged) {
            (Some(exit), Some(diverged)) => Some(exit.used.min(diverged)),
            (Some(exit), None) => Some(exit.used),
            (None, Some(diverged)) => Some(diverged),
            (None, None) => None,
        };
        let all = need(exit.as_ref(), diverged)?;
        // Every run goes once through, before any loop goes round again: what
        // that much uses, every way uses. Joining a loop's rounds can hide it.
        let once = match self.states(true) {
            Some((mut states, diverged)) => {
                let exit = states.remove(&end).map(|(state, _)| state);
                need(exit.as_ref(), diverged).unwrap_or(all)
            }
            None => all,
        };
        Some(Summary {
            need: all.max(once),
            exit,
            scrambled,
        })
    }

    /// Definition `id` checked again, given `kinds` for its inputs (the
    /// deepest first): whether every way through it fails, and what it
    /// leaves.
    fn special(&self, id: usize, kinds: Vec<Kinds>) -> Special {
        let key = (id, kinds);
        if let Ok(specials) = self.specials.try_borrow()
            && let Some(known) = specials.get(&key)
        {
            return known.clone();
        }
        let Some(code) = self.program.words.get(id) else {
            return Special::Unknown;
        };
        let mut start = State::start(false);
        let given = len(&key.1);
        start.lo = given.saturating_neg();
        start.hi = start.lo;
        start.reached = start.lo;
        start.top = key.1.clone();
        let unit = Unit::new(
            self.program,
            code,
            start,
            self.summaries,
            self.specials,
            Some(id),
        );
        let special = match unit.states(false) {
            None => Special::Unknown,
            Some((mut states, diverged)) => {
                let end = code.ops.len();
                let exit = states.remove(&end).map(|(state, _)| state);
                let scrambled = states.values().any(|(s, _)| s.scrambled);
                match (exit, diverged) {
                    (None, None) => match unit.first_error() {
                        Some(e) => Special::Fails(match e.word {
                            Some(word) => format!("{word}: {}", e.message),
                            None => e.message,
                        }),
                        None => Special::Unknown,
                    },
                    (exit, _) => Special::Does(Summary {
                        need: 0,
                        exit,
                        scrambled,
                    }),
                }
            }
        };
        if let Ok(mut specials) = self.specials.try_borrow_mut() {
            specials.insert(key, special.clone());
        }
        special
    }

    fn first_error(&self) -> Option<Error> {
        let mut first: Option<(usize, Error)> = None;
        let (states, _) = self.states(false)?;
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
            // The head of an `each`: on into the body with an element, or
            // out past the loop.
            if let Op::EachNext(out) = op {
                let mut body = state.clone();
                body.push(state.iters.last().copied().unwrap_or(ANY));
                state.iters.pop();
                return vec![(next, body), (*out, state)];
            }
            match self.step(op, &mut state) {
                Err(failing) => {
                    failed(at, self.error(at, op, failing));
                    return Vec::new();
                }
                // A way that never returns, or stops on purpose (`fail`),
                // used what it had: counted as an end, at the definition's
                // own depth (not a `list` body's).
                Ok(Flow::Diverge | Flow::Stop) => {
                    let used = state.outer.first().map_or(state.used, |o| o.used);
                    *diverged = Some(diverged.map_or(used, |d| d.min(used)));
                    return Vec::new();
                }
                Ok(Flow::Go) => {}
            }
            match op {
                Op::Jump(to) => return vec![(*to, state)],
                Op::JumpUnless(to) => return vec![(next, state.clone()), (*to, state)],
                Op::Push(_)
                | Op::Builtin(_)
                | Op::PickN(_)
                | Op::RollN(_)
                | Op::App(_)
                | Op::Call(_)
                | Op::SetLocal(_)
                | Op::GetLocal(_)
                | Op::SetGlobal(_)
                | Op::GetGlobal(_)
                | Op::Mark
                | Op::Gather
                | Op::EachStart
                | Op::EachNext(_) => {}
            }
            if self.starts.contains(&next) || next >= self.code.ops.len() {
                return vec![(next, state)];
            }
            at = next;
        }
    }

    /// One step: whether a run can go on past it.
    fn step(&self, op: &Op<A>, state: &mut State) -> Result<Flow, Failing> {
        match op {
            Op::Push(value) => state.push(Kinds::of(value)),
            Op::Builtin(builtin) => return builtin_step(*builtin, state),
            Op::PickN(n) => {
                let taken = state.pop(n.saturating_add(1))?;
                let copy = taken.first().copied().unwrap_or(ANY);
                for kinds in taken {
                    state.push(kinds);
                }
                state.push(copy);
            }
            Op::RollN(n) => {
                let taken = state.pop(n.saturating_add(1))?;
                for kinds in taken.iter().skip(1) {
                    state.push(*kinds);
                }
                state.push(taken.first().copied().unwrap_or(ANY));
            }
            Op::App(id) => {
                let (takes, gives) = self
                    .program
                    .vocabulary
                    .word(*id)
                    .map_or((0, 0..=0), |(_, takes, gives)| (takes, gives));
                state.pop(takes)?;
                let (lo, hi) = (*gives.start(), *gives.end());
                if lo == hi {
                    for _ in 0..lo {
                        state.push(ANY);
                    }
                } else {
                    let count = |n: usize| i64::try_from(n).unwrap_or(BIG);
                    state.forget(count(lo), count(hi));
                }
            }
            Op::Call(id) => {
                let Some(Some(summary)) = self.summaries.get(*id) else {
                    // Nothing known of it yet: a call to itself, the first
                    // time through.
                    return Ok(Flow::Diverge);
                };
                // Checked again with the kinds this call gives it, where
                // any are known (not for a call to itself).
                let slots = usize::try_from(summary.need).unwrap_or(0).min(GIVEN);
                let skip = state.top.len().saturating_sub(slots);
                let known = state.top.len().saturating_sub(skip);
                let mut given = vec![ANY; slots.saturating_sub(known)];
                given.extend(state.top.iter().skip(skip).copied());
                let mut special = None;
                if self.own != Some(*id) && given.iter().any(|k| *k != ANY) {
                    match self.special(*id, given.clone()) {
                        Special::Fails(why) => {
                            let said: Vec<String> = (given.iter().rev().enumerate())
                                .filter(|(_, k)| **k != ANY)
                                .map(|(i, k)| match i {
                                    0 => format!("{} on top", k.describe()),
                                    i => format!("{} {i} under the top", k.describe()),
                                })
                                .collect();
                            return Err(Failing::Type(format!(
                                "every way through it fails given {}: {why}",
                                said.join(" and ")
                            )));
                        }
                        Special::Does(does) => special = Some(does),
                        Special::Unknown => {}
                    }
                }
                state.pop(usize::try_from(summary.need).unwrap_or(0))?;
                let (exit, scrambled) = match &special {
                    Some(does) => (does.exit.as_ref(), does.scrambled || summary.scrambled),
                    None => (summary.exit.as_ref(), summary.scrambled),
                };
                // A way that went deeper than every way surely does may
                // have replaced the caller's values down to there.
                let deeper = exit.is_some_and(|exit| exit.reached.saturating_add(summary.need) < 0);
                if deeper {
                    state.forget(0, 0);
                }
                if scrambled {
                    state.forget(0, 0);
                    state.scrambled = true;
                }
                let Some(exit) = exit else {
                    return Ok(Flow::Diverge);
                };
                let lo = exit.lo.saturating_add(summary.need);
                let hi = exit.hi.saturating_add(summary.need);
                if lo != 0 || hi != 0 {
                    state.forget(lo, hi);
                }
                for kinds in &exit.top {
                    state.push(*kinds);
                }
            }
            Op::Jump(_) | Op::EachNext(_) => {}
            Op::JumpUnless(_) => flag(state)?,
            Op::SetLocal(slot) => {
                let taken = state.pop(1)?;
                name(
                    &mut state.names,
                    *slot,
                    taken.first().copied().unwrap_or(ANY),
                );
            }
            Op::GetLocal(slot) => state.push(named(&state.names, *slot)),
            Op::SetGlobal(slot) => {
                let taken = state.pop(1)?;
                if self.top {
                    let kinds = taken.first().copied().unwrap_or(ANY);
                    name(&mut state.globals, *slot, kinds);
                }
            }
            Op::GetGlobal(slot) => {
                let kinds = if self.top {
                    named(&state.globals, *slot)
                } else {
                    ANY
                };
                state.push(kinds);
            }
            Op::Mark => {
                let mut inner = State::start(true);
                inner.iters = state.iters.clone();
                inner.names = state.names.clone();
                inner.globals = state.globals.clone();
                inner.outer = std::mem::take(&mut state.outer);
                inner
                    .outer
                    .push(std::mem::replace(state, State::start(true)));
                *state = inner;
            }
            Op::Gather => {
                let mut outer = state.outer.pop().unwrap_or_else(|| {
                    let mut unknown = State::start(false);
                    unknown.forget(BIG.saturating_neg(), BIG);
                    unknown
                });
                outer.outer = std::mem::take(&mut state.outer);
                outer.iters = state.iters.clone();
                outer.names = std::mem::take(&mut state.names);
                outer.globals = std::mem::take(&mut state.globals);
                outer.scrambled = outer.scrambled || state.scrambled;
                outer.push(LIST);
                *state = outer;
            }
            Op::EachStart => {
                let taken = state.pop(1)?;
                let kinds = taken.first().copied().unwrap_or(ANY);
                if !kinds.may_be(SEQ) {
                    return Err(Failing::Type(format!(
                        "each walks a string or a list, not {}",
                        kinds.describe()
                    )));
                }
                state.iters.push(if kinds == STR { STR } else { ANY });
            }
        }
        Ok(Flow::Go)
    }

    fn error(&self, at: usize, op: &Op<A>, failing: Failing) -> Error {
        let token = self.code.tokens.get(at);
        let code = match failing {
            Failing::Underflow { .. } => Code::StackUnderflow,
            Failing::Type(_) => Code::Type,
        };
        let message = match failing {
            Failing::Underflow {
                takes,
                lo,
                hi,
                inside,
            } => {
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
                    | Op::PickN(_)
                    | Op::RollN(_)
                    | Op::App(_)
                    | Op::Jump(_)
                    | Op::JumpUnless(_)
                    | Op::SetLocal(_)
                    | Op::GetLocal(_)
                    | Op::SetGlobal(_)
                    | Op::GetGlobal(_)
                    | Op::Mark
                    | Op::Gather
                    | Op::EachStart
                    | Op::EachNext(_) => format!("takes {takes}"),
                };
                let inside = if inside {
                    " (inside list … end, which starts with an empty stack)"
                } else {
                    ""
                };
                format!("{takes} {values}, and the stack has {has} here{inside}")
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

/// Takes a boolean, as `if` and `until` do.
fn flag(state: &mut State) -> Result<(), Failing> {
    let taken = state.pop(1)?;
    match taken.as_slice() {
        [kinds] if !kinds.may_be(BOOL) => Err(Failing::Type(format!(
            "takes a boolean (from eq or lt), not {}",
            kinds.describe()
        ))),
        [_] | [] | [_, _, ..] => Ok(()),
    }
}

/// A value that must be a string or a list, for `word`.
fn seq(word: &str, kinds: Kinds) -> Result<Kinds, Failing> {
    if kinds.may_be(SEQ) {
        Ok(kinds.and(SEQ))
    } else {
        Err(Failing::Type(format!(
            "{word} takes a string or a list, not {}",
            kinds.describe()
        )))
    }
}

/// A value that must be an integer, for `word`.
fn int(word: &str, kinds: Kinds) -> Result<(), Failing> {
    if kinds.may_be(INT) {
        Ok(())
    } else {
        Err(Failing::Type(format!(
            "{word} takes an integer, not {}",
            kinds.describe()
        )))
    }
}

fn builtin_step(builtin: Builtin, state: &mut State) -> Result<Flow, Failing> {
    let name = builtin.name();
    let taken = state.pop(builtin.takes())?;
    let at = |i: usize| taken.get(i).copied().unwrap_or(ANY);
    match builtin {
        Builtin::Drop | Builtin::Print => {}
        Builtin::Fail => return Ok(Flow::Stop),
        Builtin::Pick => {
            int(name, at(0))?;
            // At least the value it copies; which one isn't known.
            let one = state.pop(1)?;
            state.push(one.first().copied().unwrap_or(ANY));
            state.push(ANY);
        }
        Builtin::Roll => {
            int(name, at(0))?;
            let one = state.pop(1)?;
            state.push(one.first().copied().unwrap_or(ANY));
            // Some value from below comes up: what was known of their
            // kinds, and of the caller's, is not any more.
            state.forget(0, 0);
            state.scrambled = true;
        }
        Builtin::Add | Builtin::Sub | Builtin::Mul | Builtin::Div => {
            let (a, b) = (at(0), at(1));
            let message = match (a.may_be(INT), b.may_be(INT)) {
                (false, false) => Some(format!("not {} and {}", a.describe(), b.describe())),
                (false, true) => Some(format!("and the one under the top is {}", a.describe())),
                (true, false) => Some(format!("and the top one is {}", b.describe())),
                (true, true) => None,
            };
            if let Some(message) = message {
                return Err(Failing::Type(format!("takes two integers, {message}")));
            }
            state.push(INT);
        }
        Builtin::Lt => {
            let (a, b) = (at(0), at(1));
            if !a.and(b).may_be(Kinds(INT.0 | SEQ.0)) {
                return Err(Failing::Type(format!(
                    "lt compares two integers, two strings or two lists, not {} and {}",
                    a.describe(),
                    b.describe()
                )));
            }
            state.push(BOOL);
        }
        Builtin::Eq => state.push(BOOL),
        Builtin::Len => {
            seq(name, at(0))?;
            state.push(INT);
        }
        Builtin::At => {
            let s = seq(name, at(0))?;
            int(name, at(1))?;
            state.push(if s == STR { STR } else { ANY });
        }
        Builtin::Slice => {
            let s = seq(name, at(0))?;
            int(name, at(1))?;
            int(name, at(2))?;
            state.push(s);
        }
        Builtin::Concat => {
            if !at(0).may_be(LIST) {
                return Err(Failing::Type(format!(
                    "concat takes a list of strings or of lists, not {}",
                    at(0).describe()
                )));
            }
            state.push(SEQ);
        }
        Builtin::Push => {
            let s = seq(name, at(0))?;
            if s == STR && !at(1).may_be(STR) {
                return Err(Failing::Type(format!(
                    "push onto a string takes a string, not {}",
                    at(1).describe()
                )));
            }
            state.push(s);
        }
        Builtin::Put => {
            let s = seq(name, at(0))?;
            int(name, at(1))?;
            if s == STR && !at(2).may_be(STR) {
                return Err(Failing::Type(format!(
                    "put into a string takes a string, not {}",
                    at(2).describe()
                )));
            }
            state.push(s);
        }
        Builtin::Code => {
            if !at(0).may_be(STR) {
                return Err(Failing::Type(format!(
                    "code takes a one-character string, not {}",
                    at(0).describe()
                )));
            }
            state.push(INT);
        }
        Builtin::Char => {
            int(name, at(0))?;
            state.push(STR);
        }
        Builtin::Kind | Builtin::String => state.push(STR),
    }
    Ok(Flow::Go)
}
