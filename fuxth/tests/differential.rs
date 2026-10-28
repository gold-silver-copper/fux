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
        let (naive, trace) = Naive::run(&text);
        // A limit each meets in its own place (a call in last place takes
        // no room in fuxth), and runs that never end, are compared no more.
        let limit = |e: &Ended| matches!(e, Ended::Failed(c, _) if c == "call_depth" || c == "stack_overflow");
        match (&ran, &naive) {
            (Ended::Refused(code, line), naive) => {
                seen.refused = seen.refused.saturating_add(1);
                // Refused only if reaching that step surely fails: there,
                // as it says, or (a call) somewhere in what it calls, the
                // first time the run reaches it. A failure later, at
                // another step, excuses nothing.
                // A refusal promises the step fails, not with which code:
                // the run may meet another mistake there first (an index
                // past the end before the element's kind).
                let failed_so = trace.fails_at(*line);
                if trace.reached.contains(line) && !failed_so {
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
                // A failure inside a library word is placed at the
                // program's step when the word is copied in, and inside
                // the library (line 0) when called: the same failure.
                let (ran, naive) = match (ran, naive) {
                    (Ended::Failed(a, x), Ended::Failed(b, y)) if *x == 0 || *y == 0 => {
                        (&Ended::Failed(a.clone(), 0), &Ended::Failed(b.clone(), 0))
                    }
                    _ => (ran, naive),
                };
                match ran {
                    Ended::Left(..) => seen.left = seen.left.saturating_add(1),
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
    assert!(
        seen.endless < usize::try_from(PROGRAMS / 10).unwrap_or(usize::MAX),
        "{seen:?}"
    );
    // Every kind of end, often.
    let share = |n: u64| usize::try_from(PROGRAMS / n).unwrap_or(usize::MAX);
    assert!(seen.left > share(5), "{seen:?}");
    assert!(seen.failed > share(20), "{seen:?}");
    assert!(seen.refused > share(10), "{seen:?}");
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
        let left @ Ended::Left(..) = fuxth(&text, u64::MAX) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        let int = |s: &str| s.parse::<i64>().is_ok();
        let mut rewritten = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            rewritten.push((*line).to_owned());
            let next_is_constant = lines.get(i.saturating_add(1)) == Some(&"let");
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
    assert!(
        checked > usize::try_from(PROGRAMS / 5).unwrap_or(usize::MAX),
        "{checked}"
    );
}

/// Whether `text` leaves `left`; or is refused for a step no run reaches,
/// a step that would surely fail but sits where nothing goes (a branch
/// whose test is always false, once a definition's body is in place of
/// its call and the depth there is known).
fn agrees(text: &str, left: &Ended) -> bool {
    match fuxth(text, u64::MAX) {
        got @ Ended::Left(..) => got == *left,
        Ended::Refused(_, line) => {
            let (naive, trace) = Naive::run(text);
            naive == *left && !trace.reached.contains(&line)
        }
        Ended::Failed(..) | Ended::Endless => false,
    }
}

/// The program with each call of its first definition replaced by the
/// definition's body, if that definition doesn't call itself.
fn inline(lines: &[&str]) -> Option<String> {
    let start = lines.iter().position(|l| *l == "define")?;
    let name = *lines.get(start.checked_add(1)?)?;
    // Its own `end`: `list`, `each` and `try` inside it close with `end` too.
    let mut depth = 0usize;
    let mut quoted = false;
    let mut end = None;
    for (i, l) in lines.iter().enumerate().skip(start) {
        if std::mem::replace(&mut quoted, false) {
            continue;
        }
        quoted = matches!(*l, "str" | "let" | "define");
        match *l {
            "define" | "list" | "each" | "try" => depth = depth.saturating_add(1),
            "end" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
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
