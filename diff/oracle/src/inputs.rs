//! The cases the oracle runs:
//!
//! - every recording in fux-vt-compare's corpus, at its size and with its
//!   resizes, each step of the recording a step here, as fux sets up a
//!   pane (8 MiB of history, fux's) and with every option off;
//! - random cases drawing from fux-vt-compare's families
//!   (`fux-vt/compare/src/families.rs`, shared as it is written there), in
//!   random sizes, histories and options, with resizes, frames, copies,
//!   and output cut anywhere;
//! - a stream that resizes often with its history full;
//! - the limits: OSC strings past their bound, links past their count and
//!   bytes, clusters past their capacity, rows past their text, and sizes
//!   too big to allocate.
use crate::case::{Case, Step};
use crate::families::{FAMILIES, RESIZE};
use crate::model::{Call, IDENTITIES, Selection, Setup};
use crate::rng::Rng;
use std::path::{Path, PathBuf};

/// Where the corpus is.
pub fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fux-vt/compare/corpus")
}

/// A step of a recording: the size it resized to, if it did, and the
/// output that followed.
pub type Piece = (Option<(u16, u16)>, Vec<u8>);

/// A recording: its name and its case.
pub struct Recording {
    pub name: String,
    pub rows: u16,
    pub cols: u16,
    pub steps: Vec<Piece>,
}

impl Recording {
    /// The recording as a case, with `history_bytes` of history and `setup`:
    /// each step's resize, then its output in pieces of 1 to `piece`
    /// bytes, as reads of a PTY come, so the screen is compared between
    /// them too, and a piece ends anywhere, inside a sequence or a
    /// character as often as not.
    pub fn case(&self, history_bytes: usize, setup: Setup, piece: usize) -> Case {
        let mut r = Rng::new(self.rows.into());
        let mut steps = Vec::new();
        for (resize, output) in &self.steps {
            if let Some((rows, cols)) = resize {
                steps.push(Step::Resize(*rows, *cols));
            }
            let mut rest = output.as_slice();
            loop {
                let size = r.below(piece).saturating_add(1);
                let (head, tail) = rest.split_at_checked(size).unwrap_or((rest, &[]));
                steps.push(Step::Output(head.to_vec()));
                rest = tail;
                if rest.is_empty() {
                    break;
                }
            }
        }
        Case {
            rows: self.rows,
            cols: self.cols,
            history_bytes,
            setup,
            steps,
        }
    }
}

fn size_of(value: Option<&serde_json::Value>) -> Option<(u16, u16)> {
    let size = value?;
    let n = |i: usize| {
        size.get(i)
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u16::try_from(n).ok())
            .filter(|&n| n > 0)
    };
    Some((n(0)?, n(1)?))
}

/// A recording, from its manifest (`NAME.json`) and bytes (`NAME.bin`), as
/// fux-vt-compare reads it (`corpus.rs`).
fn load(path: &Path) -> Result<Recording, String> {
    let at = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let text = std::fs::read_to_string(path).map_err(|e| at(&e))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| at(&e))?;
    let bytes = std::fs::read(path.with_extension("bin")).map_err(|e| at(&e))?;
    let (rows, cols) = size_of(json.get("size")).ok_or(at(&"a bad size"))?;
    let mut steps = Vec::new();
    let mut from = 0usize;
    for step in json
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .ok_or(at(&"no steps"))?
    {
        let resize = match step.get("resize") {
            Some(size) => Some(size_of(Some(size)).ok_or(at(&"a bad resize"))?),
            None => None,
        };
        let end = step
            .get("end")
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or(at(&"a step without its end"))?;
        let output = bytes.get(from..end).ok_or(at(&"a step past the bytes"))?;
        steps.push((resize, output.to_vec()));
        from = end;
    }
    if from != bytes.len() {
        return Err(at(&"bytes after the last step"));
    }
    Ok(Recording {
        name: path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        rows,
        cols,
        steps,
    })
}

/// Every recording, in name order.
pub fn corpus() -> Result<Vec<Recording>, String> {
    let dir = corpus_dir();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    paths.iter().map(|p| load(p)).collect()
}

/// fux's history for a pane, in bytes, which the corpus is run with.
pub const PANE_HISTORY_BYTES: usize = 8 << 20;

/// The history rows a budget may hold at the most, as far as a window is
/// looked up into it: 24 bytes a row, empty (`fux-vt/THREATS.md` B17), up
/// to 2,000.
fn history_rows(history_bytes: usize) -> usize {
    (history_bytes / 24).min(2_000)
}

/// A size: mostly small, so edges are near; now and then one or two
/// cells, or wide (as fux-vt-compare's cases draw them).
fn size(r: &mut Rng, small: usize, large: usize) -> u16 {
    let n = match r.below(10) {
        0 => r.below(3).saturating_add(1),
        1 => r.below(large).saturating_add(1),
        _ => r.below(small).saturating_add(2),
    };
    u16::try_from(n).unwrap_or(1)
}

/// Random options: each on half the time, or as fux sets up a pane.
fn setup(r: &mut Rng) -> Setup {
    if r.chance(25) {
        let mut pane = Setup::pane();
        pane.reflow = r.chance(70);
        return pane;
    }
    let mut setup = Setup::default();
    for (_, flag) in setup.flags() {
        *flag = r.chance(50);
    }
    if r.chance(50) {
        setup.identity = r.pick(IDENTITIES).copied();
    }
    setup
}

/// A window and a selection in it, now and then out of it, or past the
/// copy's limits; `rows` and `cols` are the screen's.
fn selection(r: &mut Rng, rows: u16, cols: u16, history_bytes: usize) -> Selection {
    let near =
        |r: &mut Rng, n: u16| u16::try_from(r.below(usize::from(n).saturating_add(3))).unwrap_or(0);
    Selection {
        offset: r.below(history_rows(history_bytes).saturating_add(3)),
        from: (near(r, rows), near(r, cols)),
        to: (near(r, rows), near(r, cols)),
        max_cells: if r.chance(30) {
            r.below(64)
        } else {
            usize::MAX
        },
        max_bytes: if r.chance(30) {
            r.below(256)
        } else {
            usize::MAX
        },
    }
}

/// Output from the families, cut anywhere into steps now and then.
fn output(r: &mut Rng, families: &[usize]) -> Vec<Vec<u8>> {
    let mut bytes = Vec::new();
    for _ in 0..r.below(10).saturating_add(1) {
        if let Some(family) = r.pick(families).and_then(|&f| FAMILIES.get(f)) {
            bytes.extend((family.generate)(r));
        }
    }
    if r.chance(20)
        && let Some((head, tail)) = bytes.split_at_checked(r.below(bytes.len()))
    {
        return vec![head.to_vec(), tail.to_vec()];
    }
    vec![bytes]
}

/// A host's call, as fux makes them between a pane's reads.
fn call(r: &mut Rng) -> Call {
    let n = r.below(256);
    let byte = u8::try_from(n).unwrap_or(0);
    let side = |n: usize| u16::try_from(n % 20).unwrap_or(0).saturating_add(1);
    match r.below(9) {
        0 => Call::HistoryBytes(*r.pick(&[0usize, 1 << 10, 64 << 10]).unwrap_or(&0)),
        1 => Call::ClearHistory,
        2 => Call::Tick,
        3 => Call::Palette(byte),
        4 => Call::Graphics(side(n), side(n / 20)),
        5 => Call::Nonce(n.is_multiple_of(2)),
        6 => Call::Submit,
        7 => Call::Record(!n.is_multiple_of(4)),
        _ => Call::Prints,
    }
}

/// A random case: output from a few families, or every one, with resizes
/// if the resize family is drawn, frames, copies, at a random size,
/// history and options.
fn random(r: &mut Rng) -> Case {
    let all: Vec<usize> = (0..FAMILIES.len()).collect();
    let mut chosen: Vec<usize> = if r.chance(20) {
        all.clone()
    } else {
        (0..r.below(3).saturating_add(1))
            .filter_map(|_| r.pick(&all).copied())
            .collect()
    };
    let resizes = chosen
        .iter()
        .any(|&f| FAMILIES.get(f).is_some_and(|f| f.name == RESIZE));
    chosen.retain(|&f| FAMILIES.get(f).is_some_and(|f| f.name != RESIZE));
    if chosen.is_empty() {
        chosen = all
            .iter()
            .copied()
            .filter(|&f| FAMILIES.get(f).is_some_and(|f| f.name != RESIZE))
            .collect();
    }
    let (rows, cols) = (size(r, 8, 40), size(r, 16, 100));
    // In bytes: none, a few rows, a block and more, many blocks, and now
    // and then fux's 8 MiB.
    let history_bytes = if r.chance(2) {
        PANE_HISTORY_BYTES
    } else {
        *r.pick(&[0usize, 0, 1 << 10, 64 << 10, 1 << 20])
            .unwrap_or(&0)
    };
    let mut steps = Vec::new();
    for _ in 0..r.below(12).saturating_add(1) {
        let roll = r.below(100);
        if (resizes && roll < 20) || roll < 5 {
            steps.push(Step::Resize(size(r, 8, 40), size(r, 16, 100)));
        } else if roll < 30 {
            steps.push(Step::Copy(selection(r, rows, cols, history_bytes)));
        } else if roll < 34 {
            steps.push(Step::Host(call(r)));
        } else if roll < 40 {
            steps.push(Step::Frame(output(r, &chosen).concat()));
        } else {
            steps.extend(output(r, &chosen).into_iter().map(Step::Output));
        }
    }
    Case {
        rows,
        cols,
        history_bytes,
        setup: setup(r),
        steps,
    }
}

/// A line of text with something of everything a reflow keeps: wide
/// glyphs, clusters, colours, links and prompt marks, long enough to wrap.
fn rich_line(r: &mut Rng, n: usize) -> Vec<u8> {
    let pieces = [
        "plain words ",
        "界界界",
        "e\u{301}",
        "👨\u{200d}👩\u{200d}👧",
        "\u{2764}\u{fe0f}",
        "\x1b[31mred\x1b[0m ",
        "\x1b[1;4:3;58:5:9mstyled\x1b[m ",
        "\x1b[48;2;1;2;3m bg \x1b[49m",
        "\x1b]8;;https://example.com/a\x1b\\link\x1b]8;;\x1b\\ ",
        "\x1b]8;id=x;https://example.com/b\x07id-link\x1b]8;;\x07 ",
        "\t",
        "0123456789",
    ];
    let mut out = format!("{n:05} ").into_bytes();
    if r.chance(10) {
        out = [b"\x1b]133;A\x07$ ".as_slice(), &out].concat();
    }
    for _ in 0..r.below(14) {
        if let Some(p) = r.pick(&pieces) {
            out.extend_from_slice(p.as_bytes());
        }
    }
    out.extend_from_slice(b"\r\n");
    out
}

/// A stream that resizes often with its history full: the history filled
/// past its limit first, then a resize between every few lines, to sizes
/// from one cell up, now and then on the alternate screen or with the
/// cursor up the screen.
fn resize_stream(r: &mut Rng) -> Case {
    let rows = u16::try_from(r.below(20).saturating_add(4)).unwrap_or(4);
    let cols = u16::try_from(r.below(70).saturating_add(10)).unwrap_or(10);
    // In bytes: a few rows, or a block or more.
    let history_bytes = *r
        .pick(&[1usize << 10, 8 << 10, 64 << 10])
        .unwrap_or(&(1 << 10));
    let mut setup = Setup::pane();
    setup.reflow = r.chance(75);
    let mut fill = Vec::new();
    let mut n = 0usize;
    // Enough lines to fill it twice over, each a record and its cells.
    let row = usize::from(cols).saturating_mul(4).saturating_add(24);
    let lines = history_bytes
        .checked_div(row)
        .unwrap_or(0)
        .saturating_mul(2);
    while n < lines.saturating_add(usize::from(rows).saturating_mul(2)) {
        fill.extend(rich_line(r, n));
        n = n.saturating_add(1);
    }
    let mut steps = vec![Step::Output(fill)];
    for _ in 0..r.below(60).saturating_add(60) {
        let (h, w) = match r.below(10) {
            0 => (1, 1),
            1 => (size(r, 3, 3), size(r, 3, 3)),
            2 => (size(r, 60, 120), size(r, 200, 250)),
            _ => (size(r, 30, 40), size(r, 100, 120)),
        };
        steps.push(Step::Resize(h, w));
        let mut out = Vec::new();
        for _ in 0..r.below(4) {
            out.extend(rich_line(r, n));
            n = n.saturating_add(1);
        }
        match r.below(12) {
            0 => out.extend_from_slice(b"\x1b[?1049h\x1b[2;3Halternate"),
            1 => out.extend_from_slice(b"\x1b[?1049l"),
            2 => out.extend_from_slice(b"\x1b[3;1Hup the screen"),
            3 => out.extend_from_slice("no line end, then a wide glyph 界".as_bytes()),
            _ => {}
        }
        steps.push(Step::Output(out));
    }
    Case {
        rows,
        cols,
        history_bytes,
        setup,
        steps,
    }
}

/// The limits: each case pushes one bound fux-vt keeps.
fn limits(r: &mut Rng, which: usize) -> Case {
    let mut setup = Setup::pane();
    setup.events = true;
    setup.extended_replies = r.chance(50);
    let repeat = |s: &str, n: usize| std::iter::repeat_n(s, n).collect::<String>();
    let (rows, cols, history_bytes, steps) = match which % 7 {
        // OSC strings at and past their bound, and DCS and APC strings
        // longer than anything kept.
        0 => {
            let mut steps = Vec::new();
            // About `OSC_PAYLOAD_LIMIT`, 65,536.
            for len in [65_534, 65_535, 65_536, 65_537, 65_636] {
                steps.push(Step::Output(
                    format!("\x1b]2;{}\x07", repeat("t", len)).into_bytes(),
                ));
                steps.push(Step::Output(
                    format!("\x1b]52;c;{}\x1b\\", repeat("QQ", len / 2)).into_bytes(),
                ));
            }
            steps.push(Step::Output(
                format!(
                    "\x1bP$q{}\x1b\\\x1b_{}\x1b\\ok",
                    repeat("m", 100),
                    repeat("a", 70_000)
                )
                .into_bytes(),
            ));
            (5, 20, 10, steps)
        }
        // Links: URIs and ids at and past their bounds, then so many
        // links, and so many bytes of them, that the screen's table fills.
        1 => {
            let mut out = Vec::new();
            for (uri, id) in [(2083, 250), (2084, 250), (2083, 251), (1, 0)] {
                out.extend(
                    format!(
                        "\x1b]8;id={};{}\x07x\x1b]8;;\x07",
                        repeat("i", id),
                        repeat("u", uri)
                    )
                    .into_bytes(),
                );
            }
            let mut many = Vec::new();
            let count = if r.chance(50) { 2_200 } else { 70_000 };
            let long = if count > 10_000 { 4 } else { 2_000 };
            for i in 0..count {
                many.extend(
                    format!("\x1b]8;;{i}{}\x07{}\x1b]8;;\x07", repeat("u", long), i % 10)
                        .into_bytes(),
                );
            }
            (6, 30, 100, vec![Step::Output(out), Step::Output(many)])
        }
        // Clusters past their capacity, and rows of long clusters past the
        // row's text.
        2 => {
            let long = format!("e{}", repeat("\u{301}", 200));
            let family = repeat("👨\u{200d}", 60);
            let row: String = (0..40)
                .map(|_| format!("a{}", repeat("\u{316}", 40)))
                .collect();
            let steps = vec![
                Step::Output(long.into_bytes()),
                Step::Output(family.into_bytes()),
                Step::Output(format!("\r\n{row}\r\n{row}").into_bytes()),
                Step::Resize(4, 13),
                Step::Output(b"\x1b[1;1H\x1b[5@\x1b[3P".to_vec()),
            ];
            (4, 20, 20, steps)
        }
        // Sizes past what a grid may hold: refused, the screen as it was.
        3 => {
            let steps = vec![
                Step::Output(b"before\r\nthe refusals".to_vec()),
                Step::Resize(1, u16::MAX),
                Step::Resize(u16::MAX, 1100),
                Step::Resize(u16::MAX, u16::MAX),
                Step::Resize(0, 5),
                Step::Resize(5, 0),
                Step::Output(b"after".to_vec()),
            ];
            (3, 10, 2_000, steps)
        }
        4 => (u16::MAX, u16::MAX, 0, vec![Step::Output(b"x".to_vec())]),
        5 => (0, 10, 0, vec![Step::Output(b"x".to_vec())]),
        // Parameters, intermediates and repeats past their bounds.
        _ => {
            let params = repeat("1;", 100);
            let steps = vec![
                Step::Output(format!("\x1b[{params}m\x1b[{params}H").into_bytes()),
                Step::Output(b"\x1b[99999999999999999999Aup\x1b[65535;65535Hcorner".to_vec()),
                Step::Output(b"x\x1b[65535b\x1b[1;1H\x1b[65535@\x1b[65535L\x1b[65535S".to_vec()),
                Step::Output(b"\x1b[!!!!!!p\x1b#############8".to_vec()),
            ];
            (8, 30, 50, steps)
        }
    };
    Case {
        rows,
        cols,
        history_bytes,
        setup,
        steps,
    }
}

/// How many kinds of limits case there are.
const LIMITS: usize = 7;

/// `n` random cases from `seed`.
pub fn random_cases(seed: u64, n: usize) -> Vec<Case> {
    let mut r = Rng::new(seed);
    (0..n).map(|_| random(&mut r)).collect()
}

/// `n` resize-heavy streams from `seed`.
pub fn resize_streams(seed: u64, n: usize) -> Vec<Case> {
    let mut r = Rng::new(seed);
    (0..n).map(|_| resize_stream(&mut r)).collect()
}

/// A case for each limit, from `seed`.
pub fn limits_cases(seed: u64) -> Vec<Case> {
    let mut r = Rng::new(seed);
    (0..LIMITS).map(|which| limits(&mut r, which)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_generated_case_is_safe_to_allocate() {
        let mut r = Rng::new(1);
        for which in 0..LIMITS {
            assert!(limits(&mut r, which).is_safe(), "limits case {which}");
        }
        for i in 0..2_000 {
            assert!(random(&mut r).is_safe(), "random case {i}");
        }
        for i in 0..20 {
            assert!(resize_stream(&mut r).is_safe(), "resize stream {i}");
        }
    }

    /// History is a budget in bytes, as fux-vt takes it: the corpus runs
    /// with fux's 8 MiB, random cases draw budgets of many blocks (a block
    /// is 4,096 cells), and a case of 1 MiB fed 20,000 lines holds rows
    /// of more than one block, compared alike.
    #[test]
    fn history_is_bytes_and_reaches_many_blocks() -> Result<(), String> {
        assert_eq!(PANE_HISTORY_BYTES, 8 << 20);
        let mut r = Rng::new(1);
        let most = (0..2_000).map(|_| random(&mut r).history_bytes).max();
        assert_eq!(most, Some(PANE_HISTORY_BYTES));
        let lines: Vec<u8> = (0..20_000)
            .flat_map(|n| format!("{n:080}\r\n").into_bytes())
            .collect();
        let case = Case {
            rows: 24,
            cols: 80,
            history_bytes: 1 << 20,
            setup: Setup::pane(),
            steps: vec![Step::Output(lines.clone())],
        };
        crate::case::check(&case).map_err(|d| d.to_string())?;
        let size = fux_vt::Size::new(24, 80).map_err(|e| e.to_string())?;
        let mut parser =
            fux_vt::Parser::new(size, case.history_bytes).map_err(|e| e.to_string())?;
        parser.process(&lines).map_err(|e| e.to_string())?;
        let held = parser.screen().history_len();
        assert!(held > 4096 / 80, "{held} rows of history, a block's worth");
        Ok(())
    }

    #[test]
    fn the_corpus_loads() -> Result<(), String> {
        let corpus = corpus()?;
        assert!(corpus.len() > 100, "{} recordings", corpus.len());
        Ok(())
    }
}
