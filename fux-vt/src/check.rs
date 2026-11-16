//! The invariant checker (THREATS.md section 6), with the `checks` feature:
//! [`crate::Parser::check`] asserts every invariant that can be read from
//! the state alone. Invariants that compare two states live in the tests
//! (`tests/corpus/invariants.rs`). The checker's own reads are not counted
//! as work. Each check is shown a state with its one fault by a test
//! beside it (`mod faults`), so a check that stops checking fails there.

use std::fmt;

/// What a check found wrong.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    /// The check's name.
    pub(crate) check: &'static str,
    /// What was wrong, and where.
    pub(crate) detail: String,
}

/// What the checker did: the checks that ran and the faults found.
#[derive(Clone, Debug, Default)]
pub struct Checked {
    ran: Vec<&'static str>,
    faults: Vec<Fault>,
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
    pub(crate) fn merge(&mut self, other: Self) {
        for check in other.ran {
            if !self.ran.contains(&check) {
                self.ran.push(check);
            }
        }
        self.faults.extend(other.faults);
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

/// What the checker's own tests read.
#[cfg(test)]
impl Checked {
    /// The checks that ran, by name.
    pub(crate) fn ran(&self) -> &[&'static str] {
        &self.ran
    }
    /// The faults found.
    pub(crate) fn faults(&self) -> &[Fault] {
        &self.faults
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

/// Runs `check` without counting its work.
pub(crate) fn unseen<T>(check: impl FnOnce() -> T) -> T {
    let before = crate::budget::spent();
    let result = check();
    crate::budget::rewind(before);
    result
}
