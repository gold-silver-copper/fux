//! The work one call does, in fux-vt's own units: the `checks` counters
//! weighted as threats.md 4.4 weighs them (`fux_vt::Work::cost`), where
//! the per-call ceiling U (`Work::CEILING`) is 50 ms on the machine the
//! weights were measured on. The working tree's fux-vt alone, set up as fux
//! sets up a pane (`Setup::pane`, 8 MiB of history):
//!
//! - every corpus recording, at its sizes, read 4 KiB at a time and each
//!   read given as fux's pane gives it, by `process_until_stop` until it is
//!   taken or a call stops at its budget: the call that did the most;
//! - a resize with its history full at 50x200, to 80 columns, and back.
//!
//! Each is held to U: past it, the part fails.
use crate::inputs::{self, Recording};
use crate::model::Setup;
use fux_vt::{Parser, Size, Work};

/// A read of a pane's output, as fux takes it from the PTY.
const READ: usize = 4096;
/// The history the resize is given, full: a pane's.
const HISTORY: usize = inputs::PANE_HISTORY_BYTES;
/// The text that fills it, in lines of 160 bytes: more than 8 MiB of
/// history holds at 200 columns.
const FILL: usize = 8 << 20;

/// The most one call did, and where.
#[derive(Debug)]
pub struct Worst {
    pub cost: u64,
    pub at: String,
}

/// A host that hears nothing.
struct Deaf;
impl fux_vt::Sink for Deaf {}

/// The options `setup` names, as the oracle's sides make them.
fn options(mut setup: Setup) -> fux_vt::Options {
    let identity = setup.identity;
    let on = fux_vt::Feature::ALL.into_iter().zip(setup.flags());
    let options: fux_vt::Options = on.filter_map(|(f, (_, on))| on.then_some(f)).collect();
    options.with_identity(identity.map(|(name, version)| fux_vt::Identity { name, version }))
}

fn size(rows: u16, cols: u16) -> Result<Size, String> {
    Size::new(rows, cols).map_err(|e| format!("{rows}x{cols}: {e}"))
}

/// `f` run, and the work it did, weighed.
fn cost<T>(f: impl FnOnce() -> T) -> (T, u64) {
    let before = Work::now();
    let out = f();
    (out, Work::now().since(before).cost())
}

/// The call that did the most over `recording`, fed as a pane is.
pub fn feed(recording: &Recording) -> Result<Worst, String> {
    let mut parser = Parser::with_options(
        size(recording.rows, recording.cols)?,
        HISTORY,
        options(Setup::pane()),
    )
    .map_err(|e| e.to_string())?;
    let mut worst = Worst {
        cost: 0,
        at: String::new(),
    };
    let mut fed = 0usize;
    for (resize, output) in &recording.steps {
        if let Some((rows, cols)) = resize {
            let (resized, done) = cost(|| {
                parser
                    .resize(size(*rows, *cols)?)
                    .map_err(|e| e.to_string())
            });
            resized?;
            if done > worst.cost {
                worst = Worst {
                    cost: done,
                    at: format!("{} resized to {rows}x{cols}", recording.name),
                };
            }
        }
        let mut reads = output.as_slice();
        while !reads.is_empty() {
            let (read, later) = reads.split_at_checked(READ).unwrap_or((reads, &[]));
            reads = later;
            let mut taken = 0usize;
            while let Some(rest) = read.get(taken..).filter(|r| !r.is_empty()) {
                let (stop, done) = cost(|| parser.process_until_stop(rest, &mut Deaf));
                if done > worst.cost {
                    worst = Worst {
                        cost: done,
                        at: format!("{} at byte {}", recording.name, fed.saturating_add(taken)),
                    };
                }
                // Stopped at a frame's edge or its budget, it is given the
                // rest; else all of it was taken, or refused (an allocation
                // past its limits, which a pane takes and goes on from).
                match stop {
                    Ok(Some((n, _))) if n > 0 => taken = taken.saturating_add(n),
                    Ok(_) | Err(_) => break,
                }
            }
            fed = fed.saturating_add(read.len());
        }
    }
    Ok(worst)
}

/// A resize with `history` bytes of history, filled by `fill` bytes of
/// text, at 50x200 to 80 columns, and back: the work of each.
pub fn resize_history(history: usize, fill: usize) -> Result<(u64, u64), String> {
    let wide = size(50, 200)?;
    let narrow = size(50, 80)?;
    let mut parser =
        Parser::with_options(wide, history, options(Setup::pane())).map_err(|e| e.to_string())?;
    let line = b"lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor \
        incididunt ut labore et dolore magna aliqua, ut enim ad minim veniam, quis nostrud\r\n";
    let mut written = 0usize;
    while written < fill {
        parser.process(line).map_err(|e| e.to_string())?;
        written = written.saturating_add(line.len());
    }
    let (there, to) = cost(|| parser.resize(narrow));
    there.map_err(|e| e.to_string())?;
    let (back, from) = cost(|| parser.resize(wide));
    back.map_err(|e| e.to_string())?;
    Ok((to, from))
}

/// Both, as the oracle's part prints it, or why one went past U.
pub fn check() -> Result<String, String> {
    let mut worst = Worst {
        cost: 0,
        at: String::new(),
    };
    for recording in inputs::corpus()? {
        let one = feed(&recording)?;
        if one.cost > worst.cost {
            worst = one;
        }
    }
    let (to, from) = resize_history(HISTORY, FILL)?;
    let share = |cost: u64| cost as f64 / Work::CEILING as f64 * 100.0;
    let summary = format!(
        "the worst 4 KiB read's call {:.2}% of U ({}); a full history resized 200 to 80 \
         columns {:.2}%, and back {:.2}%",
        share(worst.cost),
        worst.at,
        share(to),
        share(from),
    );
    match [worst.cost, to, from].iter().any(|&c| c > Work::CEILING) {
        true => Err(format!("past U: {summary}")),
        false => Ok(summary),
    }
}

#[cfg(test)]
mod tests {
    use fux_vt::Work;

    /// A recording's worst call is found and named, and a full history
    /// resized either way does work, under U.
    #[test]
    fn the_worst_call_and_a_full_resize_are_weighed() -> Result<(), String> {
        let corpus = crate::inputs::corpus()?;
        let zsh = corpus
            .iter()
            .find(|r| r.name == "zsh-resize")
            .ok_or("no zsh-resize")?;
        let worst = super::feed(zsh)?;
        assert!(worst.cost > 0 && worst.cost <= Work::CEILING, "{worst:?}");
        assert!(worst.at.starts_with("zsh-resize "), "{worst:?}");
        let (to, from) = super::resize_history(1 << 20, 2 << 20)?;
        assert!(
            to > 0 && from > 0 && to.max(from) <= Work::CEILING,
            "{to} {from}"
        );
        Ok(())
    }
}
