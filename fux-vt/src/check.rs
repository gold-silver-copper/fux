//! The invariant checker (THREATS.md section 6), with the `checks` feature:
//! [`crate::Parser::check`] asserts every invariant of the parser, both
//! screens, their history, styles, links, pictures and `Prints` that can be
//! read from the state alone, and says which checks ran. The invariants
//! that compare two states (an operation's change marks, whole against
//! chunked feeding, a rejected resize) are the tests' (`tests/corpus/
//! invariants.rs`), as they need the state before.
//!
//! Each check is a function of its own, returning what it found wrong, so
//! that a check that stops checking (its body `Ok(())`) is a mutant its
//! fault test kills. The checker does no work the work counters see: it
//! puts them back as they were.

use std::{cell::Cell, fmt};

thread_local! {
    /// How many more of `crate::reserve`'s allocations on this thread are
    /// refused (`refuse_next`).
    static REFUSE: Cell<u32> = const { Cell::new(0) };
}

/// Refuses the next `n` allocations fux-vt makes room for on this thread
/// (history's blocks, a grid's cells), as if memory had run out: for the
/// tests of what a refusal leaves (THREATS.md B44). With the `checks`
/// feature only.
pub fn refuse_next(n: u32) {
    REFUSE.set(n);
}

/// Whether this allocation is one `refuse_next` refuses; it counts one.
pub(crate) fn refusing() -> bool {
    let n = REFUSE.get();
    REFUSE.set(n.saturating_sub(1));
    n > 0
}

/// What a check found wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    /// The check's name.
    pub check: &'static str,
    /// What was wrong, and where.
    pub detail: String,
}

/// What the checker did: each check that ran, once, by name, in order; each
/// fault it found; and each invariant it cannot check yet, with what that
/// waits on.
#[derive(Clone, Debug, Default)]
pub struct Checked {
    ran: Vec<&'static str>,
    faults: Vec<Fault>,
    waiting: Vec<(&'static str, &'static str)>,
}

impl Checked {
    /// Records that `check` ran, and its fault, if it found one; `which`
    /// names the part checked (a screen), for the fault's detail.
    pub(crate) fn record(&mut self, check: &'static str, which: &str, found: Result<(), String>) {
        if !self.ran.contains(&check) {
            self.ran.push(check);
        }
        if let Err(detail) = found {
            let detail = if which.is_empty() {
                detail
            } else {
                format!("{which}: {detail}")
            };
            self.faults.push(Fault { check, detail });
        }
    }
    /// Records an invariant not checked yet, and what it waits on.
    pub(crate) fn wait(&mut self, check: &'static str, on: &'static str) {
        self.waiting.push((check, on));
    }
    /// Adds what `other` found.
    pub(crate) fn merge(&mut self, other: Self) {
        for check in other.ran {
            if !self.ran.contains(&check) {
                self.ran.push(check);
            }
        }
        self.faults.extend(other.faults);
        self.waiting.extend(other.waiting);
    }
    /// The checks that ran, by name.
    pub fn ran(&self) -> &[&'static str] {
        &self.ran
    }
    /// The faults found.
    pub fn faults(&self) -> &[Fault] {
        &self.faults
    }
    /// The invariants not checked yet, each with what it waits on.
    pub fn waiting(&self) -> &[(&'static str, &'static str)] {
        &self.waiting
    }
    /// Whether no check found a fault.
    pub fn is_clean(&self) -> bool {
        self.faults.is_empty()
    }
    /// The faults found by `check`.
    pub fn faults_of(&self, check: &str) -> impl Iterator<Item = &Fault> {
        self.faults.iter().filter(move |f| f.check == check)
    }
}

impl fmt::Display for Checked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} checks ran", self.ran.len())?;
        for fault in &self.faults {
            write!(f, "\n  {}: {}", fault.check, fault.detail)?;
        }
        Ok(())
    }
}

/// `Err(detail)` unless `holds`: one check's finding.
pub(crate) fn ensure(holds: bool, detail: impl FnOnce() -> String) -> Result<(), String> {
    if holds { Ok(()) } else { Err(detail()) }
}

/// Runs `check` with the work counters put back as they were after it: the
/// checker's own reads are no work of the terminal's.
pub(crate) fn unseen<T>(check: impl FnOnce() -> T) -> T {
    let before = crate::Work::now();
    let result = check();
    crate::work::add(|w| *w = before);
    result
}
