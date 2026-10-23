//! The bounds suite's driver: a parser set up as a row says, fed its input
//! at one chunking, each call's work and the heap's peak checked as it
//! goes against the row's bound, so that a row past its bound stops at the
//! call that crossed it rather than running away.
//!
//! With `FUX_VT_WORST_FEED=FILE`, each probe also appends to FILE its
//! costliest call and its longest, as a line `NAME CHUNK UNITS NANOSECONDS`
//! (NAME the test, the thread's name, and the probe's number in the run;
//! units weighted as `Work::cost`, in cells). With
//! `FUX_VT_WORST_FEED_INPUTS=DIR` too, a probe fed 4 KiB a call whose
//! longest call took more than [`KEPT`] leaves what it fed in DIR
//! (`NAME.bin`, each `feed` a record of its length, 8 bytes little-endian,
//! and its bytes; and `NAME.setup`: rows, columns, history and the
//! features' places in `Feature::ALL`), for fux-bench to time again
//! without the counters (`fux-bench calls`). A probe that made a call of
//! another kind (`host`, `set_host_graphics`) leaves nothing to time
//! again: its counted time stands. `fux-vt/compare/worst-feed.sh`
//! gathers both.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use fux_vt::{Feature, Options, Parser, Sink, Size, Stop, Work};

use super::Heap;

/// How large a probe's input is: its reproducer once, ten times, or a
/// hundred times (`THREATS.md` section 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    Once,
    Ten,
    Hundred,
}

impl Scale {
    /// How many times the reproducer is repeated.
    pub fn times(self) -> usize {
        match self {
            Self::Once => 1,
            Self::Ten => 10,
            Self::Hundred => 100,
        }
    }
    /// Its name in the known-failing list: `1x`, `10x` or `100x`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Once => "1x",
            Self::Ten => "10x",
            Self::Hundred => "100x",
        }
    }
}

/// A call longer than this, counted, may be past 50 ms uncounted too: its
/// probe's input is kept, to time again.
pub const KEPT: Duration = Duration::from_millis(25);

/// Probes made in this run, to name each.
static MADE: AtomicUsize = AtomicUsize::new(0);

/// The chunkings every probe feeds its input at (`THREATS.md` section 2),
/// in bytes a call: and `WHOLE`, all of it in one piece, given again from
/// each stop.
pub const CHUNKS: [usize; 4] = [1, 16, 4096, WHOLE];

/// The input in one piece, as a host reading all a program wrote at once
/// gives it.
pub const WHOLE: usize = usize::MAX;

/// `bytes` `times` times over.
pub fn repeat(bytes: &[u8], times: usize) -> Vec<u8> {
    std::iter::repeat_n(bytes, times)
        .flatten()
        .copied()
        .collect()
}

/// `bytes` in pieces of `size` (at least 1), the last maybe shorter.
pub fn pieces(mut rest: &[u8], size: usize) -> impl Iterator<Item = &[u8]> {
    let size = size.max(1);
    std::iter::from_fn(move || {
        let (piece, after) = rest.split_at_checked(size).unwrap_or((rest, &[]));
        rest = after;
        (!piece.is_empty()).then_some(piece)
    })
}

/// The parser a probe feeds: its size, its history's budget and its
/// features. fux's defaults are 50×200 and 8 MiB, with every feature on
/// (`THREATS.md` section 2).
#[derive(Clone, Copy, Debug)]
pub struct Setup {
    pub rows: u16,
    pub cols: u16,
    pub history: usize,
    pub options: Options,
}

/// fux's history budget.
pub const HISTORY: usize = 8 << 20;

impl Setup {
    /// fux's defaults.
    pub fn defaults() -> Self {
        Self {
            rows: 50,
            cols: 200,
            history: HISTORY,
            options: Feature::ALL.into_iter().collect(),
        }
    }
    /// These, at `rows` by `cols`.
    pub fn sized(self, rows: u16, cols: u16) -> Self {
        Self { rows, cols, ..self }
    }
    /// These, with `history` bytes of history.
    pub fn history(self, history: usize) -> Self {
        Self { history, ..self }
    }
    /// The screen's cells.
    pub fn cells(&self) -> usize {
        usize::from(self.rows).saturating_mul(usize::from(self.cols))
    }
    fn parser(&self) -> Result<Parser, String> {
        let size = Size::new(self.rows, self.cols).map_err(|e| e.to_string())?;
        Parser::with_options(size, self.history, self.options).map_err(|e| e.to_string())
    }
}

/// The memory formula's steady state without pictures (`THREATS.md` 3.1),
/// for a parser of `setup`: what a probe that crosses every budget but the
/// pictures' may hold above the parser as made, and the largest transient
/// while processing (3.2: the style sweep's or a shrink's, 0.5 × 1.1 × H).
pub fn steady(setup: &Setup) -> usize {
    let (n, r, c) = (
        setup.cells(),
        usize::from(setup.rows),
        usize::from(setup.cols),
    );
    let long = c.saturating_mul(32).saturating_add(128);
    let short = c.saturating_mul(17);
    let row_text = long
        .saturating_add(short)
        .saturating_mul(2)
        .saturating_add(128);
    let entries = n.saturating_mul(2).next_power_of_two().max(4096);
    let styles = entries.saturating_mul(43).saturating_add(16);
    let history = setup
        .history
        .saturating_add(setup.history / 10)
        .saturating_add(2 * 4096 * 8);
    let terms = [
        n.saturating_mul(16),
        r.saturating_mul(2 * 104),
        r.saturating_mul(2).saturating_mul(row_text),
        r.saturating_mul(2 * 256 * 48),
        2 * (4 << 20),
        styles.saturating_mul(2),
        history,
        67_000,
        6_000,
    ];
    let steady = terms.into_iter().fold(0, usize::saturating_add);
    let transient = history / 2;
    steady.saturating_add(transient)
}

/// The picture budget P (`THREATS.md` B01): 96 MiB.
pub const PICTURE_BYTES: usize = 96 << 20;

/// A bound a probe holds to (`THREATS.md` sections 2 and 4.2): for each
/// counter it names, the most per input byte, per call and per copy of
/// the reproducer; and the most bytes live above the parser as made.
/// Every call also does at most the ceiling U of weighted work (4.4).
#[derive(Clone, Copy, Debug)]
pub struct Bound {
    pub work: &'static [(&'static str, u64, u64, u64)],
    pub heap: usize,
}

/// What a probe counts beyond fux-vt's counters: the reply bytes it was
/// sent (`THREATS.md` B28).
#[derive(Default)]
struct Replies(u64);

impl Sink for Replies {
    fn reply(&mut self, bytes: &[u8]) {
        self.0 = self
            .0
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
    }
}

/// A parser being fed, with what it took so far.
pub struct Probe {
    pub parser: Parser,
    heap: Heap,
    bound: Bound,
    chunk: usize,
    copies: u64,
    /// The bytes fed and taken by host calls, the calls made, and the
    /// bytes fed to the parser alone.
    pub fed: u64,
    parsed: u64,
    pub calls: u64,
    /// The work done since the parser was made.
    pub work: Work,
    replies: Replies,
    /// The costliest call so far, in weighted work, and the longest.
    worst: (u64, Duration),
    /// Its name, its setup, and where what it is fed is kept, if it is.
    name: String,
    setup: Setup,
    kept: Option<(std::path::PathBuf, std::fs::File)>,
    /// Whether it made a call other than a feed, which a replay of its
    /// feeds would leave out.
    hosted: bool,
}

impl Probe {
    /// A parser of `setup`, to be fed at `chunk` bytes a call, held to
    /// `bound` for `copies` copies of a reproducer.
    pub fn new(setup: &Setup, bound: Bound, chunk: usize, copies: usize) -> Result<Self, String> {
        let name = format!(
            "{}-{}",
            std::thread::current().name().unwrap_or("probe"),
            MADE.fetch_add(1, Ordering::Relaxed)
        );
        // Opened before the heap is watched, and written unbuffered: the
        // probe's bound counts every allocation on its thread.
        let kept = (std::env::var_os("FUX_VT_WORST_FEED_INPUTS"))
            .filter(|_| chunk == 4096)
            .map(|dir| std::path::Path::new(&dir).join(format!("{name}.bin")))
            .and_then(|path| std::fs::File::create(&path).ok().map(|f| (path, f)));
        let parser = setup.parser()?;
        Ok(Self {
            parser,
            heap: Heap::begin(),
            bound,
            chunk: chunk.max(1),
            copies: u64::try_from(copies).unwrap_or(u64::MAX),
            fed: 0,
            parsed: 0,
            calls: 0,
            work: Work::ZERO,
            replies: Replies::default(),
            worst: (0, Duration::ZERO),
            name,
            setup: *setup,
            kept,
            hosted: false,
        })
    }

    /// Begins the part the row bounds, after setting the parser up (filling
    /// history, say): the work counted, the bytes and the calls start
    /// again, held to `bound` for `copies` copies; the heap's peak goes on.
    pub fn restart(&mut self, bound: Bound, copies: usize) {
        (self.bound, self.copies) = (bound, u64::try_from(copies).unwrap_or(u64::MAX));
        (self.fed, self.parsed, self.calls, self.work) = (0, 0, 0, Work::ZERO);
    }

    /// Feeds `bytes`, `chunk` at a time, as fux does: each piece by
    /// `process_until_stop`, given again from where a call stopped until
    /// none of it is left, each call its own. It checks after each call
    /// that it stayed under the ceiling, and that the work so far and the
    /// heap are within the bound.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), String> {
        if let Some((_, file)) = &mut self.kept {
            let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes();
            std::io::Write::write_all(file, &len).map_err(|e| e.to_string())?;
            std::io::Write::write_all(file, bytes).map_err(|e| e.to_string())?;
        }
        for piece in pieces(bytes, self.chunk) {
            let (mut rest, mut idle) = (piece, 0u32);
            while !rest.is_empty() {
                let (before, started) = (Work::now(), Instant::now());
                let stop = self
                    .parser
                    .process_until_stop(rest, &mut self.replies)
                    .map_err(|e| format!("a call failed: {e}"));
                let took = started.elapsed();
                let done = Work::now().since(before);
                self.worst = (self.worst.0.max(done.cost()), self.worst.1.max(took));
                let stop = stop?;
                let taken = stop.map_or(rest.len(), |(taken, _)| taken);
                // A call may stop at the budget before any byte, doing the
                // rows a checksum left (`Screen::resume_checksum`), spending
                // as it goes, and no more often in a row than one sequence's
                // most work allows (`idle_most`).
                idle = match taken {
                    0 => idle.saturating_add(1),
                    _ => 0,
                };
                let size = self.parser.screen().size();
                let working = matches!(stop, Some((_, Stop::Budget))) && done.cost() > 0;
                if taken == 0 && (!working || idle > idle_most(size)) {
                    return Err(format!(
                        "a call stopped before any byte, {idle} in a row, doing {}: {stop:?}",
                        done.cost()
                    ));
                }
                self.parsed = self
                    .parsed
                    .saturating_add(u64::try_from(taken).unwrap_or(u64::MAX));
                self.add(done, taken)?;
                rest = rest.get(taken..).unwrap_or_default();
            }
        }
        Ok(())
    }

    /// Runs the host call `call`, as a call of its own, held to the bound
    /// as one of `bytes` bytes.
    pub fn host<T>(
        &mut self,
        bytes: usize,
        call: impl FnOnce(&mut Parser) -> T,
    ) -> Result<T, String> {
        self.hosted = true;
        let (before, started) = (Work::now(), Instant::now());
        let result = call(&mut self.parser);
        let took = started.elapsed();
        let done = Work::now().since(before);
        self.worst = (self.worst.0.max(done.cost()), self.worst.1.max(took));
        self.add(done, bytes)?;
        Ok(result)
    }

    /// Tells the parser what graphics its host draws, before it is fed.
    pub fn set_host_graphics(&mut self, graphics: fux_vt::Graphics) {
        self.hosted = true;
        self.parser.set_host_graphics(graphics);
    }

    /// Adds a call's work, `done`, for `bytes` bytes, and checks it.
    fn add(&mut self, done: Work, bytes: usize) -> Result<(), String> {
        self.calls = self.calls.saturating_add(1);
        self.fed = self
            .fed
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        // U is the ceiling of a call of 4 KiB, as fux makes them (THREATS.md
        // 4.4), or at a screen where one screen-wide sequence costs more,
        // that sequence's most, which the parser leaves to a call of its
        // own (`Work::ceiling`): a call given more, as
        // `WHOLE` gives it, may do that for each 4 KiB, as that many calls
        // would.
        let cost = done.cost();
        let pieces = u64::try_from(bytes.div_ceil(4096).max(1)).unwrap_or(u64::MAX);
        let ceiling = Work::ceiling(self.parser.screen().size()).saturating_mul(pieces);
        if cost > ceiling {
            return Err(format!(
                "a call of {} bytes did {} weighted units, past {pieces} U = {} ({:?})",
                bytes,
                cost,
                ceiling,
                nonzero(done)
            ));
        }
        let mut sum = Work::ZERO;
        for ((name, total), (_, now)) in self.work.counters().into_iter().zip(done.counters()) {
            sum = with(sum, name, total.saturating_add(now))?;
        }
        self.work = sum;
        self.within()
    }

    /// Whether the work so far and the heap's peak are within the bound.
    fn within(&self) -> Result<(), String> {
        for &(name, per_byte, per_call, per_copy) in self.bound.work {
            let most = per_byte
                .saturating_mul(self.fed)
                .saturating_add(per_call.saturating_mul(self.calls))
                .saturating_add(per_copy.saturating_mul(self.copies));
            let done = self.counted(name)?;
            if done > most {
                return Err(format!(
                    "{name} {done} past its bound {most} ({per_byte} a byte, {per_call} a call, \
                     {per_copy} a copy) after {} bytes in {} calls",
                    self.fed, self.calls
                ));
            }
        }
        let peak = self.heap.peak();
        if peak > self.bound.heap {
            return Err(format!(
                "a heap peak of {peak} bytes past its bound {} after {} bytes",
                self.bound.heap, self.fed
            ));
        }
        Ok(())
    }

    /// What counter `name` counted so far; `replies` the reply bytes sent.
    /// A name no counter has is an error, so that a bound naming one
    /// misspelled does not pass for counting nothing.
    pub fn counted(&self, name: &str) -> Result<u64, String> {
        if name == "replies" {
            return Ok(self.replies.0);
        }
        (self.work.counters().into_iter())
            .find(|(n, _)| *n == name)
            .map(|(_, n)| n)
            .ok_or_else(|| format!("no counter is named {name}"))
    }

    /// Checks that counter `name` counted at least `least`: what the probe
    /// knows was done (`THREATS.md` 4.3), so a counter that counts nothing
    /// shows.
    pub fn at_least(&self, name: &str, least: u64) -> Result<(), String> {
        let done = self.counted(name)?;
        if done < least {
            return Err(format!(
                "{name} counted {done}, fewer than the {least} known done"
            ));
        }
        Ok(())
    }

    /// The bytes live now above the parser as made.
    pub fn live(&self) -> usize {
        self.heap.live()
    }

    /// Ends the probe: the work and the heap within the bound, at least one
    /// input byte counted for each fed, and the invariant checker clean.
    pub fn finish(&self) -> Result<(), String> {
        self.within()?;
        self.at_least("bytes", self.parsed)?;
        let report = self.parser.check();
        if !report.is_clean() {
            return Err(format!("the checker found: {report}"));
        }
        Ok(())
    }
}

impl Drop for Probe {
    /// Appends the costliest call to `FUX_VT_WORST_FEED`, if it is set;
    /// keeps what it was fed if its longest call took more than [`KEPT`].
    fn drop(&mut self) {
        if let Some((path, _)) = self.kept.take() {
            let setup = &self.setup;
            let features: Vec<String> = (Feature::ALL.into_iter().enumerate())
                .filter(|(_, f)| setup.options.has(*f))
                .map(|(i, _)| i.to_string())
                .collect();
            let text = format!(
                "{} {} {} {}\n",
                setup.rows,
                setup.cols,
                setup.history,
                features.join(",")
            );
            let _ = match self.worst.1 > KEPT && !self.hosted {
                true => std::fs::write(path.with_extension("setup"), text),
                false => std::fs::remove_file(&path),
            };
        }
        let Some(path) = std::env::var_os("FUX_VT_WORST_FEED") else {
            return;
        };
        let line = format!(
            "{} {} {} {}\n",
            self.name,
            self.chunk,
            self.worst.0,
            self.worst.1.as_nanos()
        );
        let file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path);
        if let Ok(mut file) = file {
            let _ = std::io::Write::write_all(&mut file, line.as_bytes());
        }
    }
}

/// `work` with counter `name` set to `n`; an error for a counter this
/// does not know, as one added to `Work` would be until it is.
fn with(mut work: Work, name: &str, n: u64) -> Result<Work, String> {
    let slot = match name {
        "bytes" => &mut work.bytes,
        "scanned" => &mut work.scanned,
        "cells" => &mut work.cells,
        "slots" => &mut work.slots,
        "rows" => &mut work.rows,
        "text" => &mut work.text,
        "pixels" => &mut work.pixels,
        "styles" => &mut work.styles,
        "inserts" => &mut work.inserts,
        "lookups" => &mut work.lookups,
        "links" => &mut work.links,
        "keys" => &mut work.keys,
        "sums" => &mut work.sums,
        "spans" => &mut work.spans,
        "edits" => &mut work.edits,
        _ => return Err(format!("no counter is named {name}")),
    };
    *slot = n;
    Ok(work)
}

/// The counters of `work` that counted something.
pub fn nonzero(work: Work) -> Vec<(&'static str, u64)> {
    (work.counters().into_iter())
        .filter(|(_, n)| *n > 0)
        .collect()
}

/// The most calls in a row that may stop at the budget before taking a
/// byte at a screen of `size`: those that do the rows a checksum left,
/// each spending past the budget (`Work::CEILING` halved,
/// `budget::LIMIT`), so no more than the most one such sequence costs
/// (`Work::ceiling`) allows, and one more.
pub fn idle_most(size: Size) -> u32 {
    let calls = Work::ceiling(size) / (Work::CEILING / 2);
    u32::try_from(calls).unwrap_or(u32::MAX).saturating_add(1)
}
