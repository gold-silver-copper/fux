//! B46: XParseColor's colour arithmetic (`palette/xcms.rs`), which can
//! take thousands of steps for one specification of 19 bytes. Each step
//! is a `colours` unit, spent from the call's budget; one OSC spends at
//! most its allowance, a quarter of U, past which a specification that
//! needs the arithmetic is unreadable (and ends OSC 4 and 5).

use fux_vt::{Feature, Options, Parser, Size, Work};

use super::probe::{Bound, Probe, Scale, Setup, repeat};
use super::rows::Found;

/// A 64 KiB OSC 4 of colours that each run TekHVC's chroma search to its
/// end (FA re-audit F-3: 220 to 470 ms before), ended by BEL.
fn osc() -> Vec<u8> {
    let mut one = b"\x1b]4".to_vec();
    while one.len() < (64 << 10) - 20 {
        one.extend_from_slice(b";1;TekHVC:266/48/99");
    }
    one.push(0x07);
    one
}

/// The allowance of one OSC in `colours` steps, from the budget's
/// ceiling and the counter's weight, with one specification's steps past
/// it: what is spent before the allowance is looked at again.
fn allowance() -> Result<u64, String> {
    let weight = (Work::WEIGHTS.iter())
        .find(|(name, _)| *name == "colours")
        .map(|(_, w)| *w)
        .ok_or("no counter is named colours")?;
    let allowance = Work::CEILING
        .checked_div(4)
        .and_then(|u| u.checked_div(weight));
    Ok(allowance.ok_or("a weight of 0")?.saturating_add(20_000))
}

// B46: one OSC's colour arithmetic, at most its allowance, at every
// chunking; the OSC buffer and the palette all it keeps.
pub fn b46(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    let bound = Bound {
        work: &[],
        heap: 65_537 + (16 << 10) + (4 << 10),
    };
    // Made before the probe, whose heap counts what its thread holds.
    let input = repeat(&osc(), scale.times());
    let mut probe = Probe::new(&setup, bound, chunk, scale.times())?;
    probe.feed(&input)?;
    let most = allowance()?.saturating_mul(u64::try_from(scale.times()).unwrap_or(u64::MAX));
    let done = probe.counted("colours")?;
    if done > most {
        return Err(format!("colours {done} past its bound {most}"));
    }
    probe.at_least("colours", 1)?;
    probe.finish()
}

/// The call that ends the OSC does at most U / 2 of weighted work, so the
/// budget, looked at after it, holds (budget.rs).
#[test]
fn one_osc_of_colours_is_at_most_half_the_ceiling() -> Result<(), String> {
    let _one = super::one_at_a_time();
    let options: Options = Feature::ALL.into_iter().collect();
    let size = Size::new(50, 200).map_err(|e| e.to_string())?;
    let mut parser = Parser::with_options(size, 0, options).map_err(|e| e.to_string())?;
    let osc = osc();
    let (head, last) = osc
        .split_at_checked(osc.len().saturating_sub(1))
        .ok_or("an empty OSC")?;
    parser.process(head).map_err(|e| e.to_string())?;
    let before = Work::now();
    parser.process(last).map_err(|e| e.to_string())?;
    let done = Work::now().since(before);
    let colours = done.counters().into_iter().find(|(n, _)| *n == "colours");
    assert!(colours.is_some_and(|(_, n)| n > 0), "{colours:?}");
    assert!(
        done.cost() <= Work::CEILING / 2,
        "{} past U / 2",
        done.cost()
    );
    Ok(())
}

#[test]
fn b46_1x() -> Result<(), String> {
    let _one = super::one_at_a_time();
    super::rows::run(b46, Scale::Once).map_err(|e| e.join("; "))
}

#[test]
fn b46_10x() -> Result<(), String> {
    let _one = super::one_at_a_time();
    super::rows::run(b46, Scale::Ten).map_err(|e| e.join("; "))
}
