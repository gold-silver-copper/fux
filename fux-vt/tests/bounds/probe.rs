//! The bounds suite's driver: a parser set up as a row says, fed its input
//! at one chunking, each call's work and the heap's peak checked as it
//! goes against the row's bound, so that a row past its bound stops at the
//! call that crossed it rather than running away.

use fux_vt::{Feature, Options, Parser, Sink, Size, Work};

use super::Heap;

/// How large a probe's input is: its reproducer once, ten times, or a
/// hundred times (`threats.md` section 2).
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

/// The chunkings every probe feeds its input at (`threats.md` section 2),
/// in bytes a call.
pub const CHUNKS: [usize; 3] = [1, 16, 4096];

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
/// features. fux's defaults are 50×200 and 8 MiB, with every feature on and
/// prints recorded (`threats.md` section 2's conventions).
#[derive(Clone, Copy, Debug)]
pub struct Setup {
    pub rows: u16,
    pub cols: u16,
    pub history: usize,
    pub options: Options,
    pub prints: bool,
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
            prints: true,
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
        let mut parser =
            Parser::with_options(size, self.history, self.options).map_err(|e| e.to_string())?;
        parser.record_prints(self.prints);
        Ok(parser)
    }
}

/// The memory formula's steady state without pictures (`threats.md` 3.1),
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
    let prints = n.saturating_mul(4).max(16 << 10).saturating_add(4096 * 32);
    let terms = [
        n.saturating_mul(16),
        r.saturating_mul(2 * 104),
        r.saturating_mul(2).saturating_mul(row_text),
        r.saturating_mul(2 * 256 * 48),
        2 * (4 << 20),
        styles.saturating_mul(2),
        history,
        67_000,
        prints,
        6_000,
    ];
    let steady = terms.into_iter().fold(0, usize::saturating_add);
    let transient = history / 2;
    steady.saturating_add(transient)
}

/// The picture budget P (`threats.md` P1).
pub const PICTURE_BYTES: usize = 64 << 20;

/// A bound a probe holds to (`threats.md` sections 2 and 4.2): for each
/// counter it names, the most per input byte, per call and per copy of
/// the reproducer; and the most bytes live above the parser as made.
/// Every call also does at most the ceiling U of weighted work (4.4).
#[derive(Clone, Copy, Debug)]
pub struct Bound {
    pub work: &'static [(&'static str, u64, u64, u64)],
    pub heap: usize,
}

/// What a probe counts beyond fux-vt's counters: the reply bytes it was
/// sent (`threats.md` B28).
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
}

impl Probe {
    /// A parser of `setup`, to be fed at `chunk` bytes a call, held to
    /// `bound` for `copies` copies of a reproducer.
    pub fn new(setup: &Setup, bound: Bound, chunk: usize, copies: usize) -> Result<Self, String> {
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
        })
    }

    /// Begins the part the row bounds, after setting the parser up (filling
    /// history, say): the work counted, the bytes and the calls start
    /// again, held to `bound` for `copies` copies; the heap's peak goes on.
    pub fn restart(&mut self, bound: Bound, copies: usize) {
        (self.bound, self.copies) = (bound, u64::try_from(copies).unwrap_or(u64::MAX));
        (self.fed, self.parsed, self.calls, self.work) = (0, 0, 0, Work::ZERO);
    }

    /// Feeds `bytes`, `chunk` at a time, checking after each call that it
    /// stayed under the ceiling, and that the work so far and the heap are
    /// within the bound.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), String> {
        for piece in pieces(bytes, self.chunk) {
            let before = Work::now();
            self.parser
                .process_with(piece, &mut self.replies)
                .map_err(|e| format!("a call failed: {e}"))?;
            let done = Work::now().since(before);
            self.parsed = self
                .parsed
                .saturating_add(u64::try_from(piece.len()).unwrap_or(u64::MAX));
            self.add(done, piece.len())?;
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
        let before = Work::now();
        let result = call(&mut self.parser);
        let done = Work::now().since(before);
        self.add(done, bytes)?;
        Ok(result)
    }

    /// Adds a call's work, `done`, for `bytes` bytes, and checks it.
    fn add(&mut self, done: Work, bytes: usize) -> Result<(), String> {
        self.calls = self.calls.saturating_add(1);
        self.fed = self
            .fed
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        let cost = done.cost();
        if cost > Work::CEILING {
            return Err(format!(
                "a call of {} bytes did {} weighted units, past U = {} ({:?})",
                bytes,
                cost / 20,
                Work::CEILING / 20,
                nonzero(done)
            ));
        }
        let mut sum = Work::ZERO;
        for ((name, total), (_, now)) in self.work.counters().into_iter().zip(done.counters()) {
            sum = with(sum, name, total.saturating_add(now));
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
            let done = self.counted(name);
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
    pub fn counted(&self, name: &str) -> u64 {
        if name == "replies" {
            return self.replies.0;
        }
        (self.work.counters().into_iter())
            .find(|(n, _)| *n == name)
            .map_or(0, |(_, n)| n)
    }

    /// Checks that counter `name` counted at least `least`: what the probe
    /// knows was done (`threats.md` 4.3), so a counter that counts nothing
    /// shows.
    pub fn at_least(&self, name: &str, least: u64) -> Result<(), String> {
        let done = self.counted(name);
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

/// `work` with counter `name` set to `n`.
fn with(mut work: Work, name: &str, n: u64) -> Work {
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
        "prints" => &mut work.prints,
        "keys" => &mut work.keys,
        _ => return work,
    };
    *slot = n;
    work
}

/// The counters of `work` that counted something.
pub fn nonzero(work: Work) -> Vec<(&'static str, u64)> {
    (work.counters().into_iter())
        .filter(|(_, n)| *n > 0)
        .collect()
}
