//! A case: a parser's size, history and options, and the steps both sides
//! take, each followed by a reading of everything (`observe`). Also how a
//! case is written down to replay (`Case::to_text`) and read back.
use crate::model::{Error, Heard, IDENTITIES, Selection, Setup};
use crate::observe::{self, Readers};
use crate::{Side, base, work};
use std::fmt;

/// One thing done to both parsers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Output, in one call of `process_with`.
    Output(Vec<u8>),
    /// Output through `process_until_frame`, called again after each frame
    /// it stops at, until the bytes are used up.
    Frame(Vec<u8>),
    /// A resize.
    Resize(u16, u16),
    /// A window read and a copy made from it.
    Copy(Selection),
}

/// A case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Case {
    pub rows: u16,
    pub cols: u16,
    pub history: usize,
    pub setup: Setup,
    pub steps: Vec<Step>,
}

/// How much a run compared.
#[derive(Clone, Copy, Debug, Default)]
pub struct Count {
    pub steps: u64,
    pub bytes: u64,
    pub rows: u64,
    pub cells: u64,
}

impl Count {
    pub fn add(&mut self, other: Count) {
        self.steps = self.steps.saturating_add(other.steps);
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.rows = self.rows.saturating_add(other.rows);
        self.cells = self.cells.saturating_add(other.cells);
    }
}

impl fmt::Display for Count {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} steps, {} bytes, {} rows and {} cells compared",
            self.steps, self.bytes, self.rows, self.cells
        )
    }
}

/// The first difference: after which step (none: as the parsers were
/// made), in what, and what each side gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Difference {
    pub step: Option<usize>,
    pub what: String,
    pub base: String,
    pub work: String,
}

impl fmt::Display for Difference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.step {
            Some(step) => write!(f, "after step {step}: ")?,
            None => f.write_str("as made: ")?,
        }
        write!(
            f,
            "{}\n  base: {}\n  work: {}",
            self.what, self.base, self.work
        )
    }
}

/// `Err` naming `what`, unless the two are equal.
pub fn same<T: PartialEq + fmt::Debug>(
    step: Option<usize>,
    what: &str,
    base: &T,
    work: &T,
) -> Result<(), Difference> {
    if base == work {
        return Ok(());
    }
    Err(Difference {
        step,
        what: what.into(),
        base: format!("{base:?}"),
        work: format!("{work:?}"),
    })
}

/// Two lists that should be equal: `Err` naming the first item that
/// differs, and where.
pub fn same_list<T: PartialEq + fmt::Debug>(
    step: Option<usize>,
    what: &str,
    base: &[T],
    work: &[T],
) -> Result<(), Difference> {
    if base == work {
        return Ok(());
    }
    let at = base.iter().zip(work).take_while(|(a, b)| a == b).count();
    Err(Difference {
        step,
        what: format!("{what}, item {at} of {} and {}", base.len(), work.len()),
        base: format!("{:?}", base.get(at)),
        work: format!("{:?}", work.get(at)),
    })
}

/// Runs the case on both sides, comparing everything after every step.
pub fn check(case: &Case) -> Result<Count, Difference> {
    run::<base::Terminal, work::Terminal>(case)
}

/// The most cells a case may make a grid able to hold, unless fux-vt is
/// sure to refuse the size (`grid::MAX_CELLS`, `MAX_ROWS`): so that no
/// generated or shrunk case allocates gigabytes before it is compared.
pub const SAFE_CELLS: usize = 2 << 20;
const MAX_CELLS: usize = 64 << 20;
const MAX_ROWS: usize = 1 << 20;

/// Whether a grid of this size and history is small, or certain to be
/// refused.
pub fn safe(rows: u16, cols: u16, history: usize) -> bool {
    let retained = history.saturating_add(usize::from(rows));
    let cells = retained.saturating_mul(usize::from(cols));
    cells <= SAFE_CELLS || cells > MAX_CELLS || retained > MAX_ROWS
}

impl Case {
    /// Whether every size the case asks for is safe to try.
    pub fn is_safe(&self) -> bool {
        safe(self.rows, self.cols, self.history)
            && self.steps.iter().all(|step| match step {
                Step::Resize(rows, cols) => safe(*rows, *cols, self.history),
                Step::Output(_) | Step::Frame(_) | Step::Copy(_) => true,
            })
    }

    /// The bytes of output the case gives.
    pub fn bytes(&self) -> usize {
        self.steps
            .iter()
            .map(|step| match step {
                Step::Output(b) | Step::Frame(b) => b.len(),
                Step::Resize(..) | Step::Copy(_) => 0,
            })
            .sum()
    }
}

/// The two parsers made as the case says; `Ok(None)` if both refused
/// alike.
fn make<A: Side, B: Side>(case: &Case) -> Result<Option<(A, B)>, Difference> {
    let a = A::new(case.rows, case.cols, case.history, &case.setup);
    let b = B::new(case.rows, case.cols, case.history, &case.setup);
    match (a, b) {
        (Ok(a), Ok(b)) => Ok(Some((a, b))),
        (a, b) => {
            same(
                None,
                "Parser::with_options",
                &a.as_ref().err(),
                &b.as_ref().err(),
            )?;
            Ok(None)
        }
    }
}

/// `process_until_frame` over all of `bytes`, called again after each
/// frame: what each call returned.
fn frames<S: Side>(
    s: &mut S,
    bytes: &[u8],
    heard: &mut Vec<Heard>,
) -> Vec<Result<Option<usize>, Error>> {
    let mut results = Vec::new();
    let mut rest = bytes;
    // At most a call a byte, and one more.
    for _ in 0..=bytes.len() {
        let result = s.process_until_frame(rest, heard);
        let taken = result.as_ref().ok().copied().flatten();
        results.push(result);
        match taken.and_then(|n| rest.get(n..)) {
            Some(tail) if !tail.is_empty() && tail.len() < rest.len() => rest = tail,
            _ => break,
        }
    }
    results
}

/// The marks each side keeps: the one taken as it was made, the one taken
/// after the step before, and one taken every few steps.
const MADE: usize = 0;
const LAST: usize = 1;
const OLDER: usize = 2;

fn run<A: Side, B: Side>(case: &Case) -> Result<Count, Difference> {
    let mut count = Count::default();
    let Some((mut a, mut b)) = make::<A, B>(case)? else {
        return Ok(count);
    };
    for slot in [MADE, LAST, OLDER] {
        a.take_mark(slot);
        b.take_mark(slot);
    }
    let mut readers = Readers::default();
    observe::compare(None, &mut a, &mut b, &mut readers, &mut count)?;
    for (i, step) in case.steps.iter().enumerate() {
        let at = Some(i);
        match step {
            Step::Output(bytes) => {
                let (mut ha, mut hb) = (Vec::new(), Vec::new());
                let ra = a.process(bytes, &mut ha);
                let rb = b.process(bytes, &mut hb);
                same(at, "process_with's result", &ra, &rb)?;
                same_list(at, "what process_with gave the host", &ha, &hb)?;
                count.bytes = count
                    .bytes
                    .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
            }
            Step::Frame(bytes) => {
                let (mut ha, mut hb) = (Vec::new(), Vec::new());
                let ra = frames(&mut a, bytes, &mut ha);
                let rb = frames(&mut b, bytes, &mut hb);
                same_list(at, "process_until_frame's results", &ra, &rb)?;
                same_list(at, "what process_until_frame gave the host", &ha, &hb)?;
                count.bytes = count
                    .bytes
                    .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
            }
            Step::Resize(rows, cols) => {
                let ra = a.resize(*rows, *cols);
                let rb = b.resize(*rows, *cols);
                same(at, "resize's result", &ra, &rb)?;
            }
            Step::Copy(selection) => {
                same(
                    at,
                    "a window and a copy from it",
                    &a.copy(selection, true),
                    &b.copy(selection, true),
                )?;
            }
        }
        count.steps = count.steps.saturating_add(1);
        observe::compare(at, &mut a, &mut b, &mut readers, &mut count)?;
        a.take_mark(LAST);
        b.take_mark(LAST);
        if i % 4 == 3 {
            a.take_mark(OLDER);
            b.take_mark(OLDER);
        }
    }
    Ok(count)
}

/// Bytes as a line of text: printable ASCII as it is, a backslash doubled,
/// anything else `\xNN`.
pub fn escape(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            b'\\' => out.push_str("\\\\"),
            b' '..=b'~' => out.push(char::from(b)),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

/// `escape` undone.
pub fn unescape(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len());
    let mut bytes = text.bytes();
    while let Some(b) = bytes.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        match bytes.next() {
            Some(b'\\') => out.push(b'\\'),
            Some(b'x') => {
                let hex: Vec<u8> = bytes.by_ref().take(2).collect();
                let hex = std::str::from_utf8(&hex).map_err(|e| e.to_string())?;
                out.push(u8::from_str_radix(hex, 16).map_err(|e| format!("\\x{hex}: {e}"))?);
            }
            other => return Err(format!("a backslash before {other:?}")),
        }
    }
    Ok(out)
}

impl Case {
    /// The case as lines of text, to keep and replay
    /// (`fux-vt-oracle --replay FILE`).
    pub fn to_text(&self) -> String {
        let mut out = format!(
            "size {} {}\nhistory {}\nsetup {}\n",
            self.rows, self.cols, self.history, self.setup
        );
        for step in &self.steps {
            match step {
                Step::Output(bytes) => out.push_str(&format!("output {}\n", escape(bytes))),
                Step::Frame(bytes) => out.push_str(&format!("frame {}\n", escape(bytes))),
                Step::Resize(rows, cols) => out.push_str(&format!("resize {rows} {cols}\n")),
                Step::Copy(s) => out.push_str(&format!(
                    "copy {} {} {} {} {} {} {} {} {}\n",
                    s.offset,
                    s.rows,
                    s.cols,
                    s.from.0,
                    s.from.1,
                    s.to.0,
                    s.to.1,
                    s.max_cells,
                    s.max_bytes
                )),
            }
        }
        out
    }

    /// A case `to_text` wrote.
    pub fn from_text(text: &str) -> Result<Case, String> {
        let mut case = Case {
            rows: 1,
            cols: 1,
            history: 0,
            setup: Setup::default(),
            steps: Vec::new(),
        };
        for (n, line) in text.lines().enumerate() {
            let at = |e: String| format!("line {}: {e}", n.saturating_add(1));
            let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
            let numbers = || -> Result<Vec<usize>, String> {
                rest.split_whitespace()
                    .map(|w| w.parse::<usize>().map_err(|e| at(format!("{w:?}: {e}"))))
                    .collect()
            };
            let small = |n: Option<&usize>| -> Result<u16, String> {
                n.and_then(|n| u16::try_from(*n).ok())
                    .ok_or(at("a size out of range".into()))
            };
            match word {
                "size" => {
                    let n = numbers()?;
                    case.rows = small(n.first())?;
                    case.cols = small(n.get(1))?;
                }
                "history" => case.history = *numbers()?.first().ok_or(at("no history".into()))?,
                "setup" => case.setup = setup_from(rest).map_err(at)?,
                "output" => case.steps.push(Step::Output(unescape(rest).map_err(at)?)),
                "frame" => case.steps.push(Step::Frame(unescape(rest).map_err(at)?)),
                "resize" => {
                    let n = numbers()?;
                    case.steps
                        .push(Step::Resize(small(n.first())?, small(n.get(1))?));
                }
                "copy" => {
                    let n = numbers()?;
                    let get = |i: usize| n.get(i).copied().ok_or(at("too few numbers".into()));
                    case.steps.push(Step::Copy(Selection {
                        offset: get(0)?,
                        rows: small(n.get(1))?,
                        cols: small(n.get(2))?,
                        from: (small(n.get(3))?, small(n.get(4))?),
                        to: (small(n.get(5))?, small(n.get(6))?),
                        max_cells: get(7)?,
                        max_bytes: get(8)?,
                    }));
                }
                "" => {}
                other => return Err(at(format!("no step {other:?}"))),
            }
        }
        Ok(case)
    }
}

/// The options `Setup`'s `Display` wrote.
fn setup_from(text: &str) -> Result<Setup, String> {
    let mut setup = Setup::default();
    for word in text.split(',').filter(|w| !w.is_empty()) {
        if let Some(identity) = word.strip_prefix("identity=") {
            setup.identity = Some(
                IDENTITIES
                    .iter()
                    .copied()
                    .find(|(name, version)| format!("{name}/{version}") == identity)
                    .ok_or(format!("no identity {identity:?}"))?,
            );
            continue;
        }
        let mut flags = setup.flags();
        let flag = flags
            .iter_mut()
            .find(|(name, _)| *name == word)
            .ok_or(format!("no option {word:?}"))?;
        *flag.1 = true;
    }
    Ok(setup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_case_reads_back_as_it_was_written() -> Result<(), String> {
        let case = Case {
            rows: 3,
            cols: 7,
            history: 20,
            setup: Setup::pane(),
            steps: vec![
                Step::Output(b"a\\b\x1b[1m\xff\n".to_vec()),
                Step::Frame(b"\x1b[?2026h".to_vec()),
                Step::Resize(2, 9),
                Step::Copy(Selection {
                    offset: 1,
                    rows: 2,
                    cols: 3,
                    from: (0, 1),
                    to: (1, 2),
                    max_cells: 10,
                    max_bytes: 40,
                }),
            ],
        };
        assert_eq!(Case::from_text(&case.to_text())?, case);
        Ok(())
    }

    #[test]
    fn the_same_commit_agrees_with_itself() -> Result<(), String> {
        let case = Case {
            rows: 4,
            cols: 10,
            history: 5,
            setup: Setup::pane(),
            steps: vec![
                Step::Output(b"hello\r\nworld \x1b[31mred\x1b[0m\r\n\x1b[6n".to_vec()),
                Step::Resize(3, 6),
                Step::Output(b"more text that wraps\r\n".to_vec()),
            ],
        };
        // The working tree beside itself: no difference, whatever the
        // pinned commit is.
        let count = run::<work::Terminal, work::Terminal>(&case).map_err(|d| d.to_string())?;
        assert_eq!(count.steps, 3);
        Ok(())
    }
}
