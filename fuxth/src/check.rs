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
use crate::compile::{Op, Program, Steps};
use crate::error::{Code, Error, Wants, underflow};
use crate::machine::Builtin;
use crate::value::Kind;
use crate::vocabulary::{Vocabulary, through};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::mem::take;

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

/// The kinds a value may be: a set, a bit for each `Kind`, then one for
/// each of the application's own (`Vocabulary::kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Kinds(u32);

pub(crate) const INT: Kinds = Kinds::one(Kind::Int);
const BOOL: Kinds = Kinds::one(Kind::Bool);
pub(crate) const STR: Kinds = Kinds::one(Kind::Str);
pub(crate) const LIST: Kinds = Kinds::one(Kind::List);
pub(crate) const ANY: Kinds = Kinds(u32::MAX);
/// A string or a list.
pub(crate) const SEQ: Kinds = STR.union(LIST);

impl Kinds {
    pub(crate) const NONE: Kinds = Kinds(0);

    pub(crate) const fn one(kind: Kind) -> Self {
        Self(kind.bit())
    }

    /// The application's kind `i`: the bits past `Kind::App`'s, which is
    /// a value of a kind it never named.
    pub(crate) fn host(i: usize) -> Self {
        let shift = u32::try_from(i).map_or(u32::MAX, |i| i.saturating_add(1));
        Self(Kind::App.bit().checked_shl(shift).unwrap_or(0))
    }

    pub(crate) const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub(crate) const fn and(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub(crate) const fn not(self) -> Self {
        Self(!self.0)
    }

    pub(crate) const fn may_be(self, kind: Self) -> bool {
        self.0 & kind.0 != 0
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
    /// The kinds the code's own `let` names hold, by slot: a definition's
    /// locals, or the top level's globals (a definition has no globals of
    /// its own, and may run when one has changed).
    names: Vec<Kinds>,
}

/// The kinds slot `slot` of `names` holds: any, if not known.
fn named(names: &[Kinds], slot: usize) -> Kinds {
    names.get(slot).copied().unwrap_or(ANY)
}

/// Sets slot `slot` of `names` to `kinds`.
fn name(names: &mut Vec<Kinds>, slot: usize, kinds: Kinds) {
    names.resize(names.len().max(slot.saturating_add(1)), ANY);
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
    end: End,
}

/// How the ways through a definition end, as its callers see them.
#[derive(Clone, Debug, PartialEq, Eq)]
enum End {
    /// Some way returns: the stack where they do, counted from where it
    /// was called.
    Returns(State),
    /// None returns, and some goes on without failing (a call that never
    /// returns, a loop gone round).
    Diverges,
    /// Every way stops at `fail`: a call to it is as `fail` is.
    Stops,
}

impl End {
    /// The stack where it returns; or, if it never does, how a run goes on
    /// past a call to it.
    fn exit(&self) -> Result<&State, Flow> {
        match self {
            End::Returns(exit) => Ok(exit),
            End::Diverges => Err(Flow::Diverge),
            End::Stops => Err(Flow::Stop),
        }
    }
}

/// How many of a definition's inputs a call's known kinds are given to it
/// with, to check it again for that call.
const GIVEN: usize = 8;

/// A definition checked again with the kinds a call gives it.
#[derive(Clone, Debug)]
enum Special {
    /// How it ends given them.
    Does(End),
    /// Every way through it fails given them: the first step that does.
    Fails(Box<Error>),
    /// Nothing more is known than its summary says.
    Unknown,
}

/// The definitions checked again, by definition and the kinds given.
type Specials = std::cell::RefCell<HashMap<(usize, Vec<Kinds>), Special>>;

impl Summary {
    /// What a definition might do, for one the checker can't follow.
    fn anything() -> Self {
        Self {
            need: 0,
            end: End::Returns(State::unknown()),
        }
    }
}

/// A step that surely fails: why.
enum Failing {
    /// It takes `takes`, and the stack has from `lo` to `hi`; `inside`
    /// a `list` body.
    Underflow {
        takes: usize,
        lo: i64,
        hi: i64,
        inside: bool,
    },
    /// Its values can't be the kinds it takes.
    Type(String),
    /// A call, which every way through what it calls fails: how it is
    /// given, and the first step there that fails.
    Fails(String, Box<Error>),
}

/// How the ways through a code that don't come back ended: the least any
/// had used of the entry's values, of those that went on without failing
/// (a call that never returns, a loop gone round) and of those that
/// stopped at `fail`.
#[derive(Clone, Copy, Debug, Default)]
struct Ends {
    diverged: Option<i64>,
    stopped: Option<i64>,
}

impl Ends {
    /// A way that ended as `flow` says, having used `used`.
    fn end(&mut self, used: i64, flow: Flow) {
        let least = match flow {
            Flow::Diverge => &mut self.diverged,
            Flow::Stop => &mut self.stopped,
            Flow::Go => return,
        };
        *least = Some(least.map_or(used, |u| u.min(used)));
    }

    /// How many of the entry's values every way through uses, with `exit`
    /// where the ways that return end: a way stopping at `fail` counts
    /// only if no way goes on, as a way that fails.
    fn need(&self, exit: Option<&State>) -> Option<i64> {
        let going = exit.map(|e| e.used).into_iter().chain(self.diverged).min();
        going.or(self.stopped)
    }
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
    fn start(bounded: bool) -> Self {
        Self {
            top: Vec::new(),
            lo: 0,
            hi: 0,
            used: 0,
            reached: 0,
            bounded,
            outer: Vec::new(),
            iters: Vec::new(),
            names: Vec::new(),
        }
    }

    /// A stack of which nothing is known: any depth, any kinds.
    fn unknown() -> Self {
        let mut unknown = Self::start(false);
        unknown.forget(BIG.saturating_neg(), BIG);
        unknown
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

    /// Takes one value; its kinds.
    fn pop1(&mut self) -> Result<Kinds, Failing> {
        Ok(self.pop(1)?.first().copied().unwrap_or(ANY))
    }

    /// Takes `n` values: the kinds of those among the top values, the
    /// deepest first. The others, the deepest `n` less that many, came from
    /// the rest, of any kind: none is kept for them, however many.
    fn pop(&mut self, n: usize) -> Result<Vec<Kinds>, Failing> {
        let from_rest = n.saturating_sub(self.top.len());
        let missing = count(from_rest);
        if self.bounded && self.hi.saturating_sub(missing) < 0 {
            let (lo, hi) = self.depth();
            return Err(Failing::Underflow {
                takes: n,
                lo,
                hi,
                inside: !self.outer.is_empty(),
            });
        }
        self.lo = self.lo.saturating_sub(missing);
        self.hi = self.hi.saturating_sub(missing);
        self.settle();
        let keep = self.top.len().saturating_sub(n);
        let taken = self.top.iter().skip(keep).copied().collect();
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
    fn join(&self, other: &Self) -> Self {
        let kept = self.top.len().min(other.top.len());
        let rest = |s: &Self| len(&s.top).saturating_sub(count(kept));
        let top = self.top.iter().rev().zip(other.top.iter().rev());
        let mut top: Vec<Kinds> = top.take(kept).map(|(a, b)| a.union(*b)).collect();
        top.reverse();
        // Both ways have the same `list` and `each` blocks open: blocks
        // nest, so no jump goes into or out of one.
        let outer = self.outer.iter().zip(&other.outer);
        let iters = self.iters.iter().zip(&other.iters);
        Self {
            top,
            lo: (self.lo.saturating_add(rest(self))).min(other.lo.saturating_add(rest(other))),
            hi: (self.hi.saturating_add(rest(self))).max(other.hi.saturating_add(rest(other))),
            used: self.used.min(other.used),
            reached: self.reached.min(other.reached),
            bounded: self.bounded,
            outer: outer.map(|(a, b)| a.join(b)).collect(),
            iters: iters.map(|(a, b)| a.union(*b)).collect(),
            names: join_names(&self.names, &other.names),
        }
    }

    /// `joined` (this joined with a new way), with a depth that keeps
    /// changing taken as unbounded.
    fn widen(&self, mut joined: Self) -> Self {
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
    count(top.len())
}

/// `n` as a depth: past `BIG`, unbounded.
fn count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(BIG)
}

/// Checks `program`: the first step that surely fails, if one does.
pub(crate) fn check<A>(program: &Program<A>, mut known: Known) -> Result<(), Error> {
    definitions(program, &mut known);
    let first = known.first.take();
    let main = Unit::new(program, &program.main, State::start(true), &known, None);
    if let Some(error) = earliest(first, main.first_error()) {
        return Err(error);
    }
    // A value that must be used, surely left at the end: said at the last
    // word that gives one, else the program's last.
    let words = &program.vocabulary;
    let (ops, tokens) = (&program.main.ops, &program.main.tokens);
    let exit = main
        .ends(false)
        .and_then(|(mut states, _)| states.remove(&ops.len()));
    let unused = exit.and_then(|(exit, _)| exit.top.iter().find_map(|k| words.unused(*k)));
    let Some(why) = unused else {
        return Ok(());
    };
    let gives = |op: &Op<A>| {
        let Op::App(id) = op else {
            return false;
        };
        let gives = words.word(*id).into_iter().flat_map(|w| &w.gives);
        gives
            .into_iter()
            .any(|(_, map)| words.unused(through(map, ANY)).is_some())
    };
    let at = ops
        .iter()
        .rposition(gives)
        .map_or(tokens.last(), |i| tokens.get(i));
    Err(Error::new(Code::Unused, why, at))
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
    let from = known.summaries.len();
    known.summaries.resize(program.words.len(), None);
    // A definition calling one written after it: the new ones are gone
    // over together until no summary changes, as a word calling itself is.
    let forward = (program.words.iter().enumerate().skip(from)).any(|(id, code)| {
        code.ops
            .iter()
            .any(|op| matches!(op, Op::Call(c) if *c > id))
    });
    let passes = if forward { RECURSION_ROUNDS } else { 0 };
    for pass in 0..=passes {
        let mut changed = false;
        for (id, code) in program.words.iter().enumerate().skip(from) {
            let before = known.summaries.get(id).cloned().flatten();
            summarise(program, known, id, code);
            if forward
                && pass == passes
                && let Some(slot) = known.summaries.get_mut(id)
            {
                *slot = Some(Summary::anything());
            }
            changed |= known.summaries.get(id).cloned().flatten() != before;
        }
        known.specials.get_mut().clear();
        if !changed {
            break;
        }
    }
    for (id, code) in program.words.iter().enumerate().skip(from) {
        let unit = Unit::new(program, code, State::start(false), known, Some(id));
        let error = unit.first_error();
        known.first = earliest(known.first.take(), error);
    }
}

/// Definition `id`'s summary, gone over until it stays the same, from what
/// is known of it (at first, nothing of what a call to itself does).
fn summarise<A>(program: &Program<A>, known: &mut Known, id: usize, code: &Steps<A>) {
    for round in 0..=RECURSION_ROUNDS {
        let unit = Unit::new(program, code, State::start(false), known, Some(id));
        let new = unit.run();
        let current = known.summaries.get(id).cloned().flatten();
        let next = match (&current, new) {
            (_, None) => current.clone(),
            (None, Some(new)) => Some(new),
            (Some(_), Some(_)) if round >= RECURSION_ROUNDS => Some(Summary::anything()),
            (Some(old), Some(new)) => Some(Summary {
                need: old.need.min(new.need),
                end: match (&old.end, new.end) {
                    (End::Returns(old), End::Returns(new)) => {
                        End::Returns(old.widen(old.join(&new)))
                    }
                    (End::Returns(old), End::Diverges | End::Stops) => End::Returns(old.clone()),
                    (End::Diverges | End::Stops, End::Returns(new)) => End::Returns(new),
                    (End::Stops, End::Stops) => End::Stops,
                    (End::Diverges, End::Diverges | End::Stops) | (End::Stops, End::Diverges) => {
                        End::Diverges
                    }
                },
            }),
        };
        if next == current {
            break;
        }
        if let Some(slot) = known.summaries.get_mut(id) {
            *slot = next;
        }
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
type Blocks = BTreeMap<usize, (State, u32)>;

/// One code's check: the top level, or a definition.
struct Unit<'a, A> {
    program: &'a Program<A>,
    code: &'a Steps<A>,
    /// The stack where the code starts: empty at the top level.
    start: State,
    /// What each definition does, worked out so far.
    known: &'a Known,
    /// The definition it is, if one: a call to itself uses its summary.
    /// None is the top level, whose `let` names it follows.
    own: Option<usize>,
    /// The steps where a block starts: the first, every jump's target, and
    /// each step after a jump.
    starts: BTreeSet<usize>,
}

impl<'a, A> Unit<'a, A> {
    fn words(&self) -> &'a Vocabulary {
        &self.program.vocabulary
    }

    fn new(
        program: &'a Program<A>,
        code: &'a Steps<A>,
        start: State,
        known: &'a Known,
        own: Option<usize>,
    ) -> Self {
        let mut starts = BTreeSet::from([0]);
        for (at, op) in code.ops.iter().enumerate() {
            if let Op::Jump(to) | Op::JumpUnless(to) | Op::EachNext(to) = op {
                starts.insert(*to);
                starts.insert(at.saturating_add(1));
            }
        }
        Self {
            program,
            code,
            start,
            known,
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
    fn ends(&self, forward: bool) -> Option<(Blocks, Ends)> {
        let first = (self.start.clone(), 0);
        let mut states = BTreeMap::from([(0, first)]);
        let mut todo = BTreeSet::from([0usize]);
        let mut diverged = Ends::default();
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
                    diverged.end(used, Flow::Diverge);
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
        let Some((mut states, ends)) = self.ends(false) else {
            return Some(Summary::anything());
        };
        let exit = states.remove(&end).map(|(state, _)| state);
        let all = ends.need(exit.as_ref())?;
        // Every run goes once through, before any loop goes round again: what
        // that much uses, every way uses. Joining a loop's rounds can hide it.
        let once = match self.ends(true) {
            Some((mut states, ends)) => {
                let exit = states.remove(&end).map(|(state, _)| state);
                ends.need(exit.as_ref()).unwrap_or(all)
            }
            None => all,
        };
        let end = match (exit, ends.diverged) {
            (Some(exit), _) => End::Returns(exit),
            (None, Some(_)) => End::Diverges,
            (None, None) => End::Stops,
        };
        Some(Summary {
            need: all.max(once),
            end,
        })
    }

    /// Definition `id` checked again, given `kinds` for its inputs (the
    /// deepest first): whether every way through it fails, and what it
    /// leaves.
    fn special(&self, id: usize, kinds: Vec<Kinds>) -> Special {
        let key = (id, kinds);
        if let Ok(specials) = self.known.specials.try_borrow()
            && let Some(known) = specials.get(&key)
        {
            return known.clone();
        }
        let Some(code) = self.program.words.get(id) else {
            return Special::Unknown;
        };
        // A call back into one being checked so (words calling each
        // other): nothing more is known of it there.
        if let Ok(mut specials) = self.known.specials.try_borrow_mut() {
            specials.insert(key.clone(), Special::Unknown);
        }
        let mut start = State::start(false);
        let given = len(&key.1);
        start.lo = given.saturating_neg();
        start.hi = start.lo;
        start.reached = start.lo;
        start.top = key.1.clone();
        let unit = Self::new(self.program, code, start, self.known, Some(id));
        let special = match unit.ends(false) {
            None => Special::Unknown,
            Some((mut states, ends)) => {
                let end = code.ops.len();
                let exit = states.remove(&end).map(|(state, _)| state);
                // Every way fails: by a mistake the checker is sure of, or
                // at a `fail` (with one mistake at least: a word that only
                // ever calls `fail` is called to fail).
                match (exit, ends.diverged) {
                    (None, None) => match unit.first_error() {
                        Some(e) => Special::Fails(Box::new(e)),
                        None => Special::Unknown,
                    },
                    (Some(exit), _) => Special::Does(End::Returns(exit)),
                    (None, Some(_)) => Special::Does(End::Diverges),
                }
            }
        };
        if let Ok(mut specials) = self.known.specials.try_borrow_mut() {
            specials.insert(key, special.clone());
        }
        special
    }

    fn first_error(&self) -> Option<Error> {
        let mut first: Option<(usize, Error)> = None;
        let (states, _) = self.ends(false)?;
        for (start, (state, _)) in states {
            let mut failed = |at, error| {
                if first.as_ref().is_none_or(|(seen, _)| at < *seen) {
                    first = Some((at, error));
                }
            };
            self.block(start, state, &mut failed, &mut Ends::default());
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
        diverged: &mut Ends,
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
                Ok(flow @ (Flow::Diverge | Flow::Stop)) => {
                    let used = state.outer.first().map_or(state.used, |o| o.used);
                    diverged.end(used, flow);
                    return Vec::new();
                }
                Ok(Flow::Go) => {}
            }
            // Every op named, so that a new one that jumps cannot pass here
            // as one that goes on.
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
            Op::Push(value) => state.push(Kinds::one(Kind::of(value))),
            Op::Builtin(builtin) => return builtin_step(self.words(), *builtin, state),
            // As deep as the count says, with no kind kept for each value
            // reached below the top's: those are of any kind, and stay in
            // the rest, however many.
            Op::PickN(n) => {
                let takes = n.saturating_add(1);
                let taken = state.pop(takes)?;
                let below = count(takes.saturating_sub(taken.len()));
                let copy = if below == 0 {
                    taken.first().copied().unwrap_or(ANY)
                } else {
                    state.forget(below, below);
                    ANY
                };
                for kinds in taken {
                    state.push(kinds);
                }
                state.push(copy);
            }
            Op::RollN(n) => {
                let takes = n.saturating_add(1);
                let taken = state.pop(takes)?;
                let below = count(takes.saturating_sub(taken.len()));
                let (moved, stay) = match taken.split_first() {
                    Some((moved, stay)) if below == 0 => (*moved, stay),
                    Some(_) | None => {
                        // The rest gives up the value that comes up.
                        let rest = below.saturating_sub(1);
                        state.forget(rest, rest);
                        (ANY, taken.as_slice())
                    }
                };
                for kinds in stay {
                    state.push(*kinds);
                }
                state.push(moved);
            }
            Op::App(id) => {
                let Some(word) = self.words().word(*id) else {
                    return Ok(Flow::Go);
                };
                let taken = state.pop(word.takes.len())?;
                // The `i`th value taken, the deepest first: those from the
                // rest, any.
                let below = word.takes.len().saturating_sub(taken.len());
                let at = |i: usize| (i.checked_sub(below)).and_then(|i| taken.get(i)).copied();
                for (i, (wants, said)) in word.takes.iter().enumerate() {
                    let got = at(i).unwrap_or(ANY);
                    if !got.may_be(*wants) {
                        let got = placed(self.words().describe(got), i, word.takes.len());
                        return Err(Failing::Type(format!("takes {said}, not {got}")));
                    }
                }
                for (from, map) in &word.gives {
                    state.push(through(map, from.and_then(at).unwrap_or(ANY)));
                }
            }
            Op::Call(id) => {
                let Some(Some(summary)) = self.known.summaries.get(*id) else {
                    // Nothing known of it yet: a call to itself, the first
                    // time through.
                    return Ok(Flow::Diverge);
                };
                // Too few values fail it before any kind could.
                state
                    .clone()
                    .pop(usize::try_from(summary.need).unwrap_or(0))?;
                // Checked again with the kinds this call gives it, where
                // any are known (not for a call to itself).
                // As deep as any way through it reaches, not only as deep as
                // every way does.
                let deepest = (summary.end.exit()).map_or(0, |exit| exit.reached.saturating_neg());
                let reach = deepest.max(summary.need);
                let slots = usize::try_from(reach).unwrap_or(0).min(GIVEN);
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
                                .map(|(i, k)| {
                                    let k = self.words().describe(*k);
                                    match i {
                                        0 => format!("{k} on top"),
                                        1 => format!("{k} next under it"),
                                        i => format!("{k} {i} places under the top"),
                                    }
                                })
                                .collect();
                            let given = said.join(" and ");
                            let message = format!("every way through it fails given {given}");
                            return Err(Failing::Fails(message, why));
                        }
                        Special::Does(does) => special = Some(does),
                        Special::Unknown => {}
                    }
                }
                let (before, _) = state.depth();
                state.pop(usize::try_from(summary.need).unwrap_or(0))?;
                // Depths as every call has them; kinds, where the kinds
                // this call gives tell more, as this call has them (a way
                // may leave what was given below where the stack surely
                // reaches, which this call may not have).
                let exit = match summary.end.exit() {
                    Ok(exit) => exit,
                    Err(flow) => return Ok(flow),
                };
                let refined = match special.as_ref().map(End::exit) {
                    None => None,
                    Some(Ok(known)) => {
                        let mut exit = exit.clone();
                        for (kinds, better) in exit.top.iter_mut().rev().zip(known.top.iter().rev())
                        {
                            let both = kinds.and(*better);
                            if both != Kinds(0) {
                                *kinds = both;
                            }
                        }
                        Some(exit)
                    }
                    Some(Err(flow)) => return Ok(flow),
                };
                let exit = refined.as_ref().unwrap_or(exit);
                // A way that went deeper than every way surely does may
                // have replaced the caller's values down to there: what was
                // known of their kinds is not, and the caller has reached
                // as deep, for its own callers.
                if exit.reached.saturating_add(summary.need) < 0 {
                    state.forget(0, 0);
                    state.reached = state.reached.min(before.saturating_add(exit.reached));
                }
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
            Op::JumpUnless(_) => must(self.words(), Wants::Flag, state.pop1()?, BOOL)?,
            Op::SetLocal(slot) | Op::SetGlobal(slot) => {
                let kinds = state.pop1()?;
                name(&mut state.names, *slot, kinds);
            }
            Op::GetLocal(slot) => state.push(named(&state.names, *slot)),
            Op::GetGlobal(slot) => {
                let kinds = if self.own.is_none() {
                    named(&state.names, *slot)
                } else {
                    ANY
                };
                state.push(kinds);
            }
            Op::Mark => {
                let mut inner = State::start(true);
                inner.iters = state.iters.clone();
                inner.names = state.names.clone();
                inner.outer = take(&mut state.outer);
                inner
                    .outer
                    .push(std::mem::replace(state, State::start(true)));
                *state = inner;
            }
            Op::Gather => {
                let mut outer = state.outer.pop().unwrap_or_else(State::unknown);
                outer.outer = take(&mut state.outer);
                outer.iters = state.iters.clone();
                outer.names = take(&mut state.names);
                outer.push(LIST);
                *state = outer;
            }
            Op::EachStart => {
                let kinds = state.pop1()?;
                must(self.words(), Wants::Each, kinds, SEQ)?;
                state.iters.push(if kinds == STR { STR } else { ANY });
            }
        }
        Ok(Flow::Go)
    }

    fn error(&self, at: usize, op: &Op<A>, failing: Failing) -> Error {
        let (code, message) = match failing {
            Failing::Underflow {
                takes,
                lo,
                hi,
                inside,
            } => {
                let has = if lo == hi {
                    format!("{hi} here")
                } else {
                    format!("at most {hi} here")
                };
                let verb = if let Op::Call(_) = op {
                    "needs at least"
                } else {
                    "takes"
                };
                (Code::StackUnderflow, underflow(verb, takes, &has, inside))
            }
            Failing::Type(message) => (Code::Type, message),
            Failing::Fails(message, cause) => {
                let mut error = Error::new(Code::Type, message, self.code.tokens.get(at));
                error.cause = Some(cause);
                return error;
            }
        };
        Error::new(code, message, self.code.tokens.get(at))
    }
}

/// A value of `kinds` that must be one of `may_be`, which `wants` says.
fn must(words: &Vocabulary, wants: Wants, kinds: Kinds, may_be: Kinds) -> Result<(), Failing> {
    if kinds.may_be(may_be) {
        Ok(())
    } else {
        Err(Failing::Type(wants.not(words.describe(kinds))))
    }
}

/// `got`, the kinds of the value at place `i` of `n` a word takes, the
/// deepest first, said with where it is: "a string on top".
fn placed(got: String, i: usize, n: usize) -> String {
    match (n, n.saturating_sub(i)) {
        (1, _) => got,
        (_, 1) => format!("{got} on top"),
        (_, 2) => format!("{got} under the top"),
        (_, up) => format!("{got} {} under the top", up.saturating_sub(1)),
    }
}

fn builtin_step(words: &Vocabulary, builtin: Builtin, state: &mut State) -> Result<Flow, Failing> {
    let args = builtin.args();
    let taken = state.pop(args.len())?;
    // The `i`th value taken, the deepest first: those from the rest, any.
    let below = args.len().saturating_sub(taken.len());
    let at = |i: usize| {
        (i.checked_sub(below))
            .and_then(|i| taken.get(i))
            .copied()
            .unwrap_or(ANY)
    };
    // Each place's kind, as the machine takes it, the deepest first; of
    // several, the one refused said by how far under the top it is.
    for (i, arg) in args.iter().enumerate() {
        if !at(i).may_be(arg.kinds)
            && let Some(wants) = arg.wants
        {
            let got = placed(words.describe(at(i)), i, args.len());
            return Err(Failing::Type(wants(builtin).not(got)));
        }
    }
    // What it leaves, and what one value's kind asks of another's.
    match builtin {
        Builtin::Drop | Builtin::Print => {}
        Builtin::Fail => return Ok(Flow::Stop),
        Builtin::Pick => {
            // At least the value it copies; which one isn't known.
            let one = state.pop1()?;
            state.push(one);
            state.push(ANY);
        }
        Builtin::Roll => {
            let one = state.pop1()?;
            state.push(one);
            // Some value from below comes up, from as deep as any: what was
            // known of their kinds, and of the caller's, is not any more.
            state.forget(0, 0);
            state.reached = BIG.saturating_neg();
        }
        Builtin::Add
        | Builtin::Sub
        | Builtin::Mul
        | Builtin::Div
        | Builtin::Len
        | Builtin::Code => state.push(INT),
        Builtin::Lt => {
            let (a, b) = (at(0), at(1));
            if !a.and(b).may_be(INT.union(SEQ)) {
                let got = format!("{} and {}", words.describe(a), words.describe(b));
                return Err(Failing::Type(Wants::Ordered.not(got)));
            }
            state.push(BOOL);
        }
        Builtin::Eq => state.push(BOOL),
        Builtin::At => state.push(if at(0).and(SEQ) == STR { STR } else { ANY }),
        Builtin::Slice => state.push(at(0).and(SEQ)),
        Builtin::Concat => state.push(SEQ),
        Builtin::Push => {
            let s = at(0).and(SEQ);
            if s == STR {
                must(words, Wants::PushText, at(1), STR)?;
            }
            state.push(s);
        }
        Builtin::Put => {
            let s = at(0).and(SEQ);
            if s == STR {
                must(words, Wants::PutText, at(2), STR)?;
            }
            state.push(s);
        }
        Builtin::Char | Builtin::Kind | Builtin::String => state.push(STR),
    }
    Ok(Flow::Go)
}
