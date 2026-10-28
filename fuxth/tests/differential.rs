//! Pausing and algebraic rewrites preserve a program's observable result.
mod common;
use common::{Ended, Gen, Rng, V, fuxth};

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

#[test]
fn rewrites_that_mean_the_same_end_the_same() {
    for body in [
        "1 2 add",
        "list 1 2 end 0 swap each add end",
        "true if 3 else 4 then",
        "0 begin 1 add dup 3 eq until",
        "str fail try str reason fail catch 2drop 3 end",
        "1 let x x 2 add",
    ] {
        for source in [body.to_owned(), format!("define f {body} end f")] {
            assert_eq!(
                fuxth(&source, u64::MAX),
                Ended::Left(Vec::new(), vec![V::Int(3)]),
                "{source}"
            );
        }
    }
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
            fuxth(&rewritten, u64::MAX) == left,
            "seed {seed}\n{text}\n---\n{rewritten}"
        );
        checked = checked.saturating_add(1);
    }
    assert!(
        checked > usize::try_from(PROGRAMS / 5).unwrap_or(usize::MAX),
        "{checked}"
    );
}
