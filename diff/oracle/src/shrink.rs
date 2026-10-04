//! The smallest case that still differs: steps after the difference cut,
//! then steps taken out, then bytes taken out of each step's output, then
//! sizes, history and options made smaller, round after round until
//! nothing more comes out or the time is up. Every case tried must be safe
//! to allocate (`Case::is_safe`), so making a size that was refused
//! smaller never makes one that allocates gigabytes.
use crate::case::{Case, Difference, Step, check};
use std::time::Instant;

/// Whether the candidate still differs.
fn differs(case: &Case) -> bool {
    case.is_safe() && check(case).is_err()
}

/// `items` with as many taken out as can be while `keep` holds: halves,
/// then quarters, down to one at a time.
fn reduce<T: Clone>(items: &[T], keep: &mut impl FnMut(&[T]) -> bool, until: Instant) -> Vec<T> {
    let mut items = items.to_vec();
    let mut chunk = (items.len() / 2).max(1);
    loop {
        let mut at = 0usize;
        while at < items.len() {
            if Instant::now() > until {
                return items;
            }
            let end = at.saturating_add(chunk).min(items.len());
            let mut candidate = items.get(..at).unwrap_or_default().to_vec();
            candidate.extend_from_slice(items.get(end..).unwrap_or_default());
            if keep(&candidate) {
                items = candidate;
            } else {
                at = end;
            }
        }
        if chunk == 1 {
            return items;
        }
        chunk /= 2;
    }
}

/// The case with step `i`'s output replaced.
fn with_bytes(case: &Case, i: usize, bytes: &[u8]) -> Case {
    let mut c = case.clone();
    if let Some(step) = c.steps.get_mut(i) {
        match step {
            Step::Output(b) | Step::Frame(b) => *b = bytes.to_vec(),
            Step::Resize(..) | Step::Copy(_) => {}
        }
    }
    c
}

/// Smaller versions of the case's size, history and options, one change
/// each.
fn simpler(case: &Case) -> Vec<Case> {
    let mut out = Vec::new();
    let mut push = |f: &dyn Fn(&mut Case)| {
        let mut c = case.clone();
        f(&mut c);
        if c != *case {
            out.push(c);
        }
    };
    push(&|c| c.history = 0);
    push(&|c| c.history /= 2);
    push(&|c| c.rows = (c.rows / 2).max(1));
    push(&|c| c.cols = (c.cols / 2).max(1));
    push(&|c| c.rows = c.rows.saturating_sub(1).max(1));
    push(&|c| c.cols = c.cols.saturating_sub(1).max(1));
    push(&|c| c.setup.identity = None);
    for flag in 0..12 {
        push(&|c| {
            if let Some((_, on)) = c.setup.flags().into_iter().nth(flag) {
                *on = false;
            }
        });
    }
    for i in 0..case.steps.len() {
        push(&|c| {
            if let Some(Step::Resize(rows, _)) = c.steps.get_mut(i) {
                *rows = (*rows / 2).max(1);
            }
        });
        push(&|c| {
            if let Some(Step::Resize(_, cols)) = c.steps.get_mut(i) {
                *cols = (*cols / 2).max(1);
            }
        });
        push(&|c| {
            if let Some(step @ Step::Frame(_)) = c.steps.get_mut(i)
                && let Step::Frame(bytes) = step.clone()
            {
                *step = Step::Output(bytes);
            }
        });
    }
    out
}

/// The smallest case found by `until` that still differs from `case`,
/// which differs (`first`, its difference).
pub fn shrink(case: &Case, first: &Difference, until: Instant) -> Case {
    let mut best = case.clone();
    // Nothing after the step that differed matters.
    if let Some(step) = first.step {
        let mut cut = best.clone();
        cut.steps.truncate(step.saturating_add(1));
        if differs(&cut) {
            best = cut;
        }
    } else {
        let mut cut = best.clone();
        cut.steps.clear();
        if differs(&cut) {
            best = cut;
        }
    }
    loop {
        let before = best.clone();
        let steps = reduce(
            &best.steps,
            &mut |steps| {
                let mut c = best.clone();
                c.steps = steps.to_vec();
                differs(&c)
            },
            until,
        );
        best.steps = steps;
        for i in 0..best.steps.len() {
            let bytes = match best.steps.get(i) {
                Some(Step::Output(b) | Step::Frame(b)) => b.clone(),
                Some(Step::Resize(..) | Step::Copy(_)) | None => continue,
            };
            let reduced = reduce(
                &bytes,
                &mut |bytes| differs(&with_bytes(&best, i, bytes)),
                until,
            );
            best = with_bytes(&best, i, &reduced);
        }
        for candidate in simpler(&best) {
            if Instant::now() > until {
                break;
            }
            if differs(&candidate) {
                best = candidate;
            }
        }
        if best == before || Instant::now() > until {
            return best;
        }
    }
}
