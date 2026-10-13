//! fuxth held to a second interpreter on random programs: the same ends,
//! paused at every step or not, and a program refused before it runs only
//! when the step it names would surely fail.
mod common;
use common::{Ended, Gen, Naive, Rng, fuxth};

/// Programs per test: enough to reach every word in many places, quick in
/// a debug build.
const PROGRAMS: u64 = 10_000;

/// A random program: every other one typed, to run far.
fn program(rng: &mut Rng, seed: u64) -> String {
    if seed.is_multiple_of(2) {
        Gen::new(rng).program()
    } else {
        Gen::new(rng).typed_program()
    }
}

/// What happened over many programs, to show the test reached each kind.
#[derive(Default, Debug)]
struct Seen {
    left: usize,
    failed: usize,
    refused: usize,
    endless: usize,
}

#[test]
fn fuxth_ends_as_the_naive_interpreter_does() {
    let mut seen = Seen::default();
    let mut wrong = Vec::new();
    for seed in 0..PROGRAMS {
        let mut rng = Rng::new(seed);
        let text = program(&mut rng, seed);
        let ran = fuxth(&text, u64::MAX);
        let (naive, reached) = Naive::run(&text);
        // A limit each meets in its own place (a call in last place takes
        // no room in fuxth), and runs that never end, are compared no more.
        let limit = |e: &Ended| matches!(e, Ended::Failed(c, _) if c == "call_depth" || c == "stack_overflow");
        match (&ran, &naive) {
            (Ended::Refused(code, line), naive) => {
                seen.refused = seen.refused.saturating_add(1);
                // Refused only if reaching that step surely fails: there,
                // as it says, or (a call) somewhere in what it calls.
                let either = ["type", "stack_underflow"];
                let failed_so = match naive {
                    // At the step itself, too few values or the wrong kind,
                    // as the depth it is called at has it.
                    Ended::Failed(c, at) => {
                        at != line
                            || c == code
                            || (either.contains(&&**c) && either.contains(&&**code))
                    }
                    Ended::Left(_) | Ended::Refused(..) | Ended::Endless => false,
                };
                if reached.contains(line) && !failed_so {
                    wrong.push(format!(
                        "seed {seed}: refused {code} at line {line}, which ran: {naive:?}\n{text}"
                    ));
                }
                if !matches!(code.as_str(), "stack_underflow" | "type" | "overflow") {
                    wrong.push(format!("seed {seed}: refused for {code}\n{text}"));
                }
            }
            (Ended::Endless, _) | (_, Ended::Endless) => {
                seen.endless = seen.endless.saturating_add(1);
            }
            (ran, naive) if limit(ran) || limit(naive) => {
                seen.endless = seen.endless.saturating_add(1);
            }
            (ran, naive) => {
                match ran {
                    Ended::Left(_) => seen.left = seen.left.saturating_add(1),
                    Ended::Failed(..) | Ended::Refused(..) | Ended::Endless => {
                        seen.failed = seen.failed.saturating_add(1);
                    }
                }
                if ran != naive {
                    wrong.push(format!(
                        "seed {seed}: fuxth {ran:?}, naive {naive:?}\n{text}"
                    ));
                }
            }
        }
        if wrong.len() > 3 {
            break;
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n\n"));
    assert!(seen.endless < 1000, "{seen:?}");
    // Every kind of end, often.
    assert!(seen.left > 2000, "{seen:?}");
    assert!(seen.failed > 500, "{seen:?}");
    assert!(seen.refused > 1000, "{seen:?}");
}

/// Stopping after every step, and going on, changes nothing: every step,
/// suspension and error happens as in one run.
#[test]
fn pausing_at_every_step_changes_nothing() {
    for seed in 0..PROGRAMS {
        let mut rng = Rng::new(seed.wrapping_add(1 << 32));
        let text = program(&mut rng, seed);
        let whole = fuxth(&text, u64::MAX);
        if whole == Ended::Endless {
            continue;
        }
        // Near the steps allowed, the paused run's count, kept per pause,
        // may call it endless first.
        for budget in [1, 7] {
            let paused = fuxth(&text, budget);
            if paused != Ended::Endless {
                assert_eq!(paused, whole, "seed {seed}, budget {budget}\n{text}");
            }
        }
    }
}

/// Rewrites that keep a program's meaning keep its end: `dup drop`, `swap
/// swap`, `0 add` and `1 mul` after values they fit, and a definition's
/// body in place of a call to it.
#[test]
fn rewrites_that_mean_the_same_end_the_same() {
    let mut checked = 0usize;
    for seed in 0..PROGRAMS {
        let mut rng = Rng::new(seed.wrapping_add(2 << 32));
        let text = program(&mut rng, seed);
        let Ended::Left(left) = fuxth(&text, u64::MAX) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        let int = |s: &str| s.parse::<i64>().is_ok();
        let mut rewritten = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            rewritten.push((*line).to_owned());
            let next_is_constant = lines.get(i.saturating_add(1)) == Some(&"constant");
            let quoted = i > 0 && lines.get(i.saturating_sub(1)) == Some(&"str");
            if !int(line) || quoted || next_is_constant {
                continue;
            }
            let extra: &[&str] = match rng.below(4) {
                0 => &["dup", "drop"],
                1 => &["0", "add"],
                2 => &["1", "mul"],
                _ => &["dup", "swap", "swap", "drop"],
            };
            rewritten.extend(extra.iter().map(|s| (*s).to_owned()));
        }
        let rewritten = rewritten.join("\n");
        assert!(
            agrees(&rewritten, &left),
            "seed {seed}\n{text}\n---\n{rewritten}"
        );
        if let Some(inlined) = inline(&lines) {
            assert!(
                agrees(&inlined, &left),
                "seed {seed}\n{text}\n---\n{inlined}"
            );
        }
        checked = checked.saturating_add(1);
    }
    assert!(checked > 2000, "{checked}");
}

/// Whether `text` leaves `left`; or is refused for a step no run reaches,
/// a step that would surely fail but sits where nothing goes (a branch
/// whose test is always false, once a definition's body is in place of
/// its call and the depth there is known).
fn agrees(text: &str, left: &[common::V]) -> bool {
    match fuxth(text, u64::MAX) {
        Ended::Left(got) => got == left,
        Ended::Refused(_, line) => {
            let (naive, reached) = Naive::run(text);
            naive == Ended::Left(left.to_vec()) && !reached.contains(&line)
        }
        Ended::Failed(..) | Ended::Endless => false,
    }
}

/// The program with each call of its first definition replaced by the
/// definition's body, if that definition doesn't call itself.
fn inline(lines: &[&str]) -> Option<String> {
    let start = lines.iter().position(|l| *l == "define")?;
    let name = *lines.get(start.checked_add(1)?)?;
    let end = start.checked_add(lines.iter().skip(start).position(|l| *l == "end")?)?;
    let body = lines.get(start.checked_add(2)?..end)?;
    if body.contains(&name) {
        return None;
    }
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let quoted = i > 0 && lines.get(i.saturating_sub(1)) == Some(&"str");
        if *line == name && i > end && !quoted {
            out.extend(body.iter().copied());
        } else {
            out.push(*line);
        }
    }
    Some(out.join("\n"))
}
