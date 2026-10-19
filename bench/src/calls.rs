//! `fux-bench calls`: what fux-vt's bounds suite kept of a probe whose
//! call took long with its work counters on (`FUX_VT_WORST_FEED_INPUTS`,
//! `fux-vt/tests/bounds/probe.rs`), fed again here, in fux's own build
//! without them: a parser of the probe's setup, the input 4 KiB a call,
//! each call by `process_until_stop` until its piece is taken, each timed.
//! The longest call of each, best of three runs, is what fux would take.
use std::path::Path;
use std::time::{Duration, Instant};

/// A probe's kept setup, as `probe.rs` writes it: rows, columns, history,
/// prints and the features' places in `Feature::ALL`.
fn parser(setup: &str) -> Result<fux_vt::Parser, String> {
    let words: Vec<&str> = setup.split_whitespace().collect();
    let [rows, cols, history, prints, rest @ ..] = words.as_slice() else {
        return Err(format!("not a probe's setup: {setup:?}"));
    };
    let number = |w: &str| w.parse::<usize>().map_err(|e| format!("{w:?}: {e}"));
    let rows = u16::try_from(number(rows)?).map_err(|e| e.to_string())?;
    let cols = u16::try_from(number(cols)?).map_err(|e| e.to_string())?;
    let all: Vec<fux_vt::Feature> = fux_vt::Feature::ALL.into_iter().collect();
    let mut features = Vec::new();
    for i in rest
        .iter()
        .flat_map(|w| w.split(','))
        .filter(|w| !w.is_empty())
    {
        features.push(*all.get(number(i)?).ok_or(format!("no feature {i}"))?);
    }
    let options: fux_vt::Options = features.into_iter().collect();
    let size = fux_vt::Size::new(rows, cols).map_err(|e| e.to_string())?;
    let mut parser =
        fux_vt::Parser::with_options(size, number(history)?, options).map_err(|e| e.to_string())?;
    parser.record_prints(*prints == "true");
    Ok(parser)
}

/// The longest call over `input`, fed to a parser of `setup`.
fn longest(setup: &str, input: &[u8]) -> Result<Duration, String> {
    struct Deaf;
    impl fux_vt::Sink for Deaf {}
    let mut parser = parser(setup)?;
    let mut most = Duration::ZERO;
    let mut reads = input;
    while !reads.is_empty() {
        let (read, later) = reads.split_at_checked(4096).unwrap_or((reads, &[]));
        reads = later;
        let mut rest = read;
        while !rest.is_empty() {
            let started = Instant::now();
            let stop = parser.process_until_stop(std::hint::black_box(rest), &mut Deaf);
            most = most.max(started.elapsed());
            match stop {
                Ok(Some((taken, _))) if taken > 0 => rest = rest.get(taken..).unwrap_or_default(),
                Ok(_) | Err(_) => break,
            }
        }
    }
    Ok(most)
}

/// Each kept probe in `dir`, its longest call, best of three: printed as
/// `NAME 4096 0 NANOSECONDS`, the bounds suite's lines with no units.
pub fn run(dir: &Path) -> Result<bool, String> {
    let mut kept: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "setup"))
        .collect();
    kept.sort();
    for path in kept {
        let setup =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let bin = path.with_extension("bin");
        let input = std::fs::read(&bin).map_err(|e| format!("{}: {e}", bin.display()))?;
        let mut best = Duration::MAX;
        for _ in 0..3 {
            best = best.min(longest(&setup, &input)?);
        }
        let name = path
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        println!("{name} 4096 0 {}", best.as_nanos());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    /// A setup as the bounds suite writes it makes that parser, and its
    /// input is fed and timed; a setup that is not one is refused.
    #[test]
    fn a_kept_probe_is_fed_again_as_it_was_set_up() -> Result<(), String> {
        let parser = super::parser("50 200 8388608 true 0,1,2\n")?;
        assert_eq!(
            parser.screen().size(),
            fux_vt::Size::new(50, 200).map_err(|e| e.to_string())?
        );
        assert!(super::longest("4 10 0 false", &[b'x'; 10_000])? > std::time::Duration::ZERO);
        assert!(super::parser("50 200").is_err());
        assert!(super::parser("50 200 0 true 9999").is_err());
        Ok(())
    }
}
