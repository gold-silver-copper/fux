//! The smallest case that still differs: steps after the difference cut,
//! then steps taken out, then bytes taken out of each step's output, then
//! sizes, history and options made smaller, round after round until
//! nothing more comes out or the time is up. Every case tried must be safe
//! to allocate (`Case::is_safe`), so making a size that was refused
//! smaller never makes one that allocates gigabytes.
use crate::case::{Case, Count, Difference, Step, check};
use std::time::Instant;

/// What a difference is in, its numbers left out: shrinking moves rows,
/// columns and items, so a candidate that differs in the same field at
/// another place still reproduces it, and one that differs in another
/// field is another difference.
fn field(difference: &Difference) -> String {
    (difference.what.chars())
        .filter(|c| !c.is_ascii_digit())
        .collect()
}

/// How a case is run and compared: `check`, or a test's stand-in.
type Check<'a> = &'a dyn Fn(&Case) -> Result<Count, Difference>;

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
            Step::Resize(..) | Step::Copy(_) | Step::Host(_) => {}
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
    push(&|c| c.history_bytes = 0);
    push(&|c| c.history_bytes /= 2);
    push(&|c| c.rows = (c.rows / 2).max(1));
    push(&|c| c.cols = (c.cols / 2).max(1));
    push(&|c| c.rows = c.rows.saturating_sub(1).max(1));
    push(&|c| c.cols = c.cols.saturating_sub(1).max(1));
    push(&|c| c.setup.identity = None);
    let flags = case.clone().setup.flags().len();
    for flag in 0..flags {
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
/// which differs (`first`, its difference), in the same field.
pub fn shrink(case: &Case, first: &Difference, until: Instant) -> Case {
    shrink_by(case, first, until, &check)
}

/// `shrink`, each case run and compared by `check`.
fn shrink_by(case: &Case, first: &Difference, until: Instant, check: Check<'_>) -> Case {
    let kind = field(first);
    let differs = |case: &Case| case.is_safe() && check(case).is_err_and(|d| field(&d) == kind);
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
                Some(Step::Resize(..) | Step::Copy(_) | Step::Host(_)) | None => continue,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Setup;

    /// A stand-in for the two sides: an output holding `b` differs in a
    /// row's cell, one holding `a` alone in the cursor.
    fn sides(case: &Case) -> Result<Count, Difference> {
        let holds =
            |byte| (case.steps.iter()).any(|s| matches!(s, Step::Output(b) if b.contains(&byte)));
        let differ = |what: &str| Difference {
            step: Some(0),
            what: what.into(),
            base: String::new(),
            work: String::new(),
        };
        if holds(b'b') {
            let at = case.bytes();
            Err(differ(&format!("row {at} from the bottom: cell")))
        } else if holds(b'a') {
            Err(differ("cursor"))
        } else {
            Ok(Count::default())
        }
    }

    /// A case that differs in two fields shrinks to one that still differs
    /// in the first one found, wherever it now is, not to the smaller one
    /// that differs in the other; and the palette, the last of the setup's
    /// flags, is turned off too.
    #[test]
    fn shrinking_keeps_the_field_that_differed() {
        let mut setup = Setup::pane();
        setup.palette = true;
        let case = Case {
            rows: 4,
            cols: 10,
            history_bytes: 0,
            setup,
            steps: vec![Step::Output(b"xxbxxxxaxx".to_vec())],
        };
        let first = sides(&case).err();
        let until = Instant::now() + std::time::Duration::from_secs(5);
        let small = first.map(|first| shrink_by(&case, &first, until, &sides));
        assert_eq!(
            small.map(|c| (c.steps, c.setup.palette)),
            Some((vec![Step::Output(b"b".to_vec())], false))
        );
    }
}
