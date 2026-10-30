//! The bounds suite's driver: a parser set up as a row says, fed its input
//! at one chunking, each call's work checked against the quota and the
//! heap's peak against the row's bound as it goes, so that a row past its
//! bound stops at the call that crossed it rather than running away.
//!
//! With `FUX_VT_WORST_FEED=FILE`, each probe also appends to FILE its
//! costliest call and its longest, as a line `NAME CHUNK UNITS NANOSECONDS`
//! (NAME the test, the thread's name, and the probe's number in the run;
//! units as `fux_vt::work_spent` counts them). With
//! `FUX_VT_WORST_FEED_INPUTS=DIR` too, a probe fed 4 KiB a call whose
//! longest call took more than [`KEPT`] leaves what it fed in DIR
//! (`NAME.bin`, each `feed` a record of its length, 8 bytes little-endian,
//! and its bytes; and `NAME.setup`: rows, columns, history and the
//! features' places in `Feature::ALL`), for fux-bench to time again
//! without the `checks` feature (`fux-bench calls`). A probe that made a call of
//! another kind (`host`, `set_host_graphics`) leaves nothing to time
//! again: its counted time stands. `fux-vt/compare/worst-feed.sh`
//! gathers both.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use fux_vt::{Feature, Options, Parser, Sink, Size, Stop};

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
    /// Its name in a failure: `1x`, `10x` or `100x`.
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
    /// A parser of these, its host's cells 10 by 20 pixels, as a terminal
    /// that says its cell size gives fux: without one, no picture is placed.
    fn parser(&self) -> Result<Parser, String> {
        let size = Size::new(self.rows, self.cols).map_err(|e| e.to_string())?;
        let mut parser =
            Parser::with_options(size, self.history, self.options).map_err(|e| e.to_string())?;
        parser.set_host_graphics(
            fux_vt::CellSize::new(10, 20).map_or(fux_vt::Graphics::Text, fux_vt::Graphics::Sixel),
        );
        Ok(parser)
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

/// What a call of `process_until_stop` may spend before it stops, over as
/// many calls as that takes: the quota, `budget::LIMIT` (`THREATS.md` 4).
pub const LIMIT: u64 = 1_000_000;

/// The most one indivisible operation spends at a parser of `setup`
/// (`THREATS.md` 4.2): what a call may spend past [`LIMIT`].
pub fn most(setup: &Setup) -> u64 {
    most_at(setup.rows, setup.cols, setup.history)
}

/// [`most`] at a screen of `rows` by `cols` and `history` bytes of
/// history (`fux_vt::call_most`, less the quota).
pub fn most_at(rows: u16, cols: u16, history: usize) -> u64 {
    Size::new(rows, cols).map_or(u64::MAX, |size| {
        fux_vt::call_most(size, history).saturating_sub(LIMIT)
    })
}

/// The largest [`most`], at fux's largest pane, 4,096 × 4,096, and its
/// largest history, 1 GiB (`THREATS.md` 1.2 and 4.2).
pub const LARGEST: u64 = 2_013_047_090;

/// A bound a probe holds to (`THREATS.md` section 2): the most work it
/// spends a byte, a call and a copy of the reproducer, for a row that
/// bounds a rate, amortized; and the most bytes live above the parser as
/// made. Every call is held to the quota besides ([`most`]).
#[derive(Clone, Copy, Debug)]
pub struct Bound {
    pub work: Option<(u64, u64, u64)>,
    pub heap: usize,
}

/// What a probe counts beyond fux-vt's work: the reply bytes it was sent
/// (`THREATS.md` B28).
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
    /// The bytes fed and taken by host calls, and the calls made.
    pub fed: u64,
    pub calls: u64,
    /// The work done since the parser was made, and since the last budget
    /// stop.
    pub work: u64,
    since_stop: u64,
    replies: Replies,
    /// The costliest call so far and the longest.
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
            calls: 0,
            work: 0,
            since_stop: 0,
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
        (self.fed, self.calls, self.work) = (0, 0, 0);
    }

    /// Feeds `bytes`, `chunk` at a time, as fux does: each piece by
    /// `process_until_stop`, given again from where a call stopped until
    /// none of it is left, each call its own; then calls of no bytes while
    /// work is left. It checks after each call that it spent at most the
    /// quota and one indivisible operation, that a budget stop came no
    /// sooner than the quota, that a call of no byte did some work, and
    /// that the work so far and the heap are within the bound.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), String> {
        if let Some((_, file)) = &mut self.kept {
            let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes();
            std::io::Write::write_all(file, &len).map_err(|e| e.to_string())?;
            std::io::Write::write_all(file, bytes).map_err(|e| e.to_string())?;
        }
        for piece in pieces(bytes, self.chunk) {
            self.parse(piece)?;
        }
        self.parse(&[])
    }

    /// `feed`'s calls for one piece, and those of no bytes after it while
    /// work is left.
    fn parse(&mut self, piece: &[u8]) -> Result<(), String> {
        let (mut rest, mut idle) = (piece, 0u64);
        while !rest.is_empty() || self.parser.has_work_left() {
            let (before, started) = (fux_vt::work_spent(), Instant::now());
            let stop = self
                .parser
                .process_until_stop(rest, &mut self.replies)
                .map_err(|e| format!("a call failed: {e}"));
            let took = started.elapsed();
            let done = fux_vt::work_spent().saturating_sub(before);
            self.worst = (self.worst.0.max(done), self.worst.1.max(took));
            let stop = stop?;
            let taken = stop.map_or(rest.len(), |(taken, _)| taken);
            let most = LIMIT.saturating_add(most(&self.setup));
            if done > most {
                return Err(format!(
                    "a call of {} bytes spent {done}, past the quota and an indivisible \
                     operation, {most}",
                    rest.len()
                ));
            }
            // A budget stop comes once the quota is spent since the last,
            // over as many calls as that took, frame boundaries between,
            // and no sooner; between two, at most the quota and one
            // operation are spent.
            let owed = self.since_stop >= LIMIT;
            self.since_stop = self.since_stop.saturating_add(done);
            if self.since_stop > most {
                return Err(format!(
                    "{} spent since the last budget stop, past {most}",
                    self.since_stop
                ));
            }
            let budget = matches!(stop, Some((_, Stop::Budget)));
            if budget {
                if self.since_stop < LIMIT {
                    return Err(format!("a budget stop after {} spent", self.since_stop));
                }
                self.since_stop = 0;
            }
            // A call that takes no byte goes on with what the last left,
            // spending, no more often in a row than a REP's copies allow,
            // or makes the stop a frame's boundary owed.
            idle = match taken {
                0 => idle.saturating_add(1),
                _ => 0,
            };
            let paid = done > 0 || budget && owed;
            if taken == 0 && (!paid || idle > IDLE_MOST) {
                return Err(format!(
                    "a call stopped before any byte, {idle} in a row, spending {done}: {stop:?}"
                ));
            }
            self.add(done, taken)?;
            rest = rest.get(taken..).unwrap_or_default();
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
        let (before, started) = (fux_vt::work_spent(), Instant::now());
        let result = call(&mut self.parser);
        let took = started.elapsed();
        let done = fux_vt::work_spent().saturating_sub(before);
        self.worst = (self.worst.0.max(done), self.worst.1.max(took));
        self.add(done, bytes)?;
        Ok(result)
    }

    /// Tells the parser what graphics its host draws, before it is fed.
    pub fn set_host_graphics(&mut self, graphics: fux_vt::Graphics) {
        self.hosted = true;
        self.parser.set_host_graphics(graphics);
    }

    /// Adds a call's work, `done`, for `bytes` bytes, and checks it.
    fn add(&mut self, done: u64, bytes: usize) -> Result<(), String> {
        self.calls = self.calls.saturating_add(1);
        self.fed = self
            .fed
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        self.work = self.work.saturating_add(done);
        self.within()
    }

    /// Whether the work so far and the heap's peak are within the bound.
    fn within(&self) -> Result<(), String> {
        if let Some((per_byte, per_call, per_copy)) = self.bound.work {
            let most = per_byte
                .saturating_mul(self.fed)
                .saturating_add(per_call.saturating_mul(self.calls))
                .saturating_add(per_copy.saturating_mul(self.copies));
            if self.work > most {
                return Err(format!(
                    "{} spent, past its bound {most} ({per_byte} a byte, {per_call} a call, \
                     {per_copy} a copy) after {} bytes in {} calls",
                    self.work, self.fed, self.calls
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

    /// The reply bytes it was sent so far.
    pub fn replies(&self) -> u64 {
        self.replies.0
    }

    /// Checks that the probe spent at least `least` since it began: what
    /// it knows was done (`THREATS.md` 4.2), so work that goes unspent
    /// shows.
    pub fn at_least(&self, least: u64) -> Result<(), String> {
        if self.work < least {
            return Err(format!(
                "{} spent, less than the {least} known done",
                self.work
            ));
        }
        Ok(())
    }

    /// The bytes live now above the parser as made.
    pub fn live(&self) -> usize {
        self.heap.live()
    }

    /// Ends the probe: the work and the heap within the bound, and the
    /// invariant checker clean.
    pub fn finish(&self) -> Result<(), String> {
        self.within()?;
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

/// The most calls in a row that may stop at the budget before taking a
/// byte: those that make the copies a REP left, each at least one of at
/// most 65,535, then the one that finishes them and the stop a frame's
/// boundary owed.
pub const IDLE_MOST: u64 = 65_535 + 2;
