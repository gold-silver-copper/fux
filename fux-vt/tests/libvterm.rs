//! libvterm's own tests (`tests/data/libvterm`, libvterm 0.3.3's `t/`,
//! MIT, copied unchanged), translated: their screen and cursor assertions,
//! run on fux-vt.
//!
//! The files are in the language of libvterm's `t/run-test.pl`, which this
//! reads as that script does: `PUSH` writes bytes (a Perl string, with its
//! `.` and `x`), `RESET`, `RESIZE`, `UTF8` and `WANTSCREEN` set the
//! terminal up as `t/harness.c` does, `$SEQ` and `$REP` repeat a line, and
//! the assertions are checked as the harness answers them:
//!
//! - `?cursor`: the cursor, row and column from 0; while a wrap is pending
//!   it is in the last column.
//! - `?screen_chars`, `?screen_row` and `?screen_text`: the text in a
//!   rectangle as `vterm_screen_get_chars` reads it. An erased cell is a
//!   space only when text follows it in its row, a printed space always is,
//!   the second half of a wide glyph is nothing, and rows are joined by a
//!   line feed. `?screen_text` is that text in UTF-8.
//! - `?screen_eol`: whether a cell and every cell after it in its row are
//!   erased (`vterm_screen_is_eol`).
//! - `?screen_cell`: a cell's characters and its width, as
//!   `vterm_screen_get_cell` gives them, at most six characters
//!   (`VTERM_MAX_CHARS_PER_CELL`: the API's cell has room for no more).
//!   The rendition and colours it also prints are not checked: they are
//!   libvterm's palette and pen, which `?pen` tests too.
//!
//! What the harness's setup means for fux-vt: `RESET` is a new terminal of
//! the current size (libvterm's `vterm_state_reset` and
//! `vterm_screen_reset`, hard); `WANTSCREEN r` is reflow
//! ([`Feature::Reflow`]); and `WANTSCREEN b` is the harness's scrollback,
//! whose every row, when a taller screen asks for rows back, is `ABCDE`
//! (`screen_sb_popline`). So fux-vt keeps no history, except that a
//! resize to more rows with `b` set first gives it a history of `ABCDE`
//! rows, written before the rest of the test since its last reset, which
//! is written again. The callback expectations (`putglyph`, `damage` …),
//! `?pen`, `?lineinfo` and `?screen_attrs_extent` are libvterm's own API
//! and are not checked; files with nothing checked are not copied.
//!
//! The same translation drives libvterm itself, its `t/harness.c` built
//! from its source, so a translation that misreads a test shows there:
//! `libvterm_passes_its_own_tests`, run with the harness's path in
//! `FUX_VT_LIBVTERM_HARNESS` (built in libvterm 0.3.3's source by
//! `cc -std=c99 -D_XOPEN_SOURCE=600 -Iinclude -Isrc src/*.c t/harness.c
//! -o harness`).
//!
//! `tests/data/libvterm/expected.txt` lists the assertions fux-vt fails,
//! each with its reason: an authority that decides otherwise, or a choice
//! the README records. A failure not listed fails the test, and so does a
//! listed one that passes.

use fux_vt::{Feature, Options, Parser, Size};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// libvterm's `VTERM_MAX_CHARS_PER_CELL`.
const MAX_CHARS: usize = 6;
/// What libvterm's cell holds in the second half of a wide glyph.
const CONTINUATION: u32 = u32::MAX;
/// The file the first translation left out, needing reflow: the other
/// files' checks are the 384 the suite was first scored on.
const REFLOW: &str = "69screen_reflow.test";

/// A command of the test language, as `t/harness.c` reads it.
#[derive(Clone, Debug)]
enum Op {
    Init,
    State,
    Screen(String),
    Utf8(bool),
    Reset,
    Resize(u16, u16),
    Push(Vec<u8>),
}

/// A terminal the tests run on.
trait Term {
    fn op(&mut self, op: &Op) -> Result;
    fn cursor(&mut self) -> Result<(u16, u16)>;
    /// A cell's characters: none when erased, [`CONTINUATION`] for the
    /// second half of a wide glyph.
    fn cell(&mut self, row: u16, col: u16) -> Result<Vec<u32>>;
}

fn data() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/libvterm")
}

/// A Perl expression as the tests write one: double-quoted strings joined
/// by `.`, each perhaps repeated by `x N`. Its characters, by code.
fn perl(expr: &str) -> Result<Vec<u32>> {
    let mut out = Vec::new();
    let mut rest = expr.trim();
    loop {
        let body = rest
            .strip_prefix('"')
            .ok_or_else(|| format!("no string: {expr}"))?;
        let (string, after) = perl_string(body)?;
        rest = after.trim_start();
        let mut times = 1;
        if let Some(count) = rest.strip_prefix('x') {
            let count = count.trim_start();
            let digits = count
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(count.len());
            let (n, after) = count.split_at_checked(digits).ok_or("a count")?;
            times = n.parse()?;
            rest = after.trim_start();
        }
        for _ in 0..times {
            out.extend_from_slice(&string);
        }
        match rest.strip_prefix('.') {
            Some(more) => rest = more.trim_start(),
            None if rest.is_empty() => return Ok(out),
            None => return Err(format!("left over: {rest}").into()),
        }
    }
}

/// A double-quoted string's characters, up to its closing quote, and what
/// follows it.
fn perl_string(body: &str) -> Result<(Vec<u32>, &str)> {
    let mut out = Vec::new();
    let mut chars = body.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Ok((out, body.get(i.saturating_add(1)..).unwrap_or(""))),
            '\\' => {
                let (_, e) = chars.next().ok_or("a lone backslash")?;
                let code = match e {
                    'e' => 0x1b,
                    'r' => 0x0d,
                    'n' => 0x0a,
                    't' => 0x09,
                    'a' => 0x07,
                    'b' => 0x08,
                    '0' => 0,
                    'x' => {
                        let hex: String = if chars.clone().next().map(|(_, c)| c) == Some('{') {
                            chars.next();
                            chars
                                .by_ref()
                                .map(|(_, c)| c)
                                .take_while(|&c| c != '}')
                                .collect()
                        } else {
                            let ahead = chars.as_str();
                            let n = ahead
                                .chars()
                                .take(2)
                                .take_while(char::is_ascii_hexdigit)
                                .count();
                            chars.by_ref().take(n).map(|(_, c)| c).collect()
                        };
                        u32::from_str_radix(&hex, 16)?
                    }
                    other => u32::from(other),
                };
                out.push(code);
            }
            _ => out.push(u32::from(c)),
        }
    }
    Err(format!("no closing quote: {body}").into())
}

/// Characters as bytes, for `PUSH`: Perl's `unpack "H*"`.
fn bytes(chars: &[u32]) -> Result<Vec<u8>> {
    Ok(chars
        .iter()
        .map(|&c| u8::try_from(c))
        .collect::<std::result::Result<_, _>>()?)
}

/// `0x41,0x42` and the like.
fn hex_list(list: &str) -> Result<Vec<u32>> {
    list.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(|x| Ok(u32::from_str_radix(x.trim_start_matches("0x"), 16)?))
        .collect()
}

fn numbers<const N: usize>(args: &str) -> Result<[u16; N]> {
    let v: Vec<u16> = args
        .split(',')
        .map(|x| x.trim().parse())
        .collect::<std::result::Result<_, _>>()?;
    v.try_into()
        .map_err(|v| format!("{N} numbers, not {v:?}").into())
}

/// One assertion, read from its line.
#[derive(Debug)]
struct Check {
    /// `FILE:LINE`.
    at: String,
    kind: String,
    args: String,
    want: String,
}

/// A line of a test, read as `run-test.pl`'s `do_line` reads it: commands,
/// and the checks this translation runs.
enum Line {
    Op(Op),
    Check(Check),
}

fn read_line(line: &str, at: &str, out: &mut Vec<Line>) -> Result {
    if let Some(rest) = line.strip_prefix("$SEQ") {
        let (range, body) = rest.split_once(':').ok_or("a $SEQ range")?;
        let mut range = range.split_whitespace();
        let low: u32 = range.next().ok_or("a $SEQ low")?.parse()?;
        let high: u32 = range.next().ok_or("a $SEQ high")?.parse()?;
        for value in low..=high {
            read_line(
                &body.trim_start().replace("\\#", &value.to_string()),
                at,
                out,
            )?;
        }
    } else if let Some(rest) = line.strip_prefix("$REP") {
        let (count, body) = rest.split_once(':').ok_or("a $REP count")?;
        for _ in 0..count.trim().parse::<u32>()? {
            read_line(body.trim_start(), at, out)?;
        }
    } else if let Some(rest) = line.strip_prefix('?') {
        let (assertion, want) = rest.split_once('=').ok_or("an assertion's =")?;
        let assertion = assertion.trim_end();
        let (kind, args) = assertion.split_once(' ').unwrap_or((assertion, ""));
        let checked = [
            "cursor",
            "screen_chars",
            "screen_row",
            "screen_text",
            "screen_eol",
            "screen_cell",
        ];
        if checked.contains(&kind) {
            out.push(Line::Check(Check {
                at: at.to_owned(),
                kind: kind.to_owned(),
                args: args.trim().to_owned(),
                want: want.trim().to_owned(),
            }));
        }
    } else if line.starts_with(|c: char| c.is_ascii_uppercase()) {
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        let op = match word {
            "INIT" => Op::Init,
            "WANTSTATE" => Op::State,
            "WANTSCREEN" => Op::Screen(rest.trim().to_owned()),
            "UTF8" => Op::Utf8(rest.trim() == "1"),
            "RESET" => Op::Reset,
            "RESIZE" => {
                let [rows, cols] = numbers(rest)?;
                Op::Resize(rows, cols)
            }
            "PUSH" => Op::Push(bytes(&perl(rest)?)?),
            // Damage reports and default colours: nothing a check here reads.
            "DAMAGEMERGE" | "DAMAGEFLUSH" | "SETDEFAULTCOL" => return Ok(()),
            _ => return Err(format!("{at}: no translation for {line}").into()),
        };
        out.push(Line::Op(op));
    }
    // Titles (`!`) and callback expectations (lowercase) are not checked.
    Ok(())
}

fn read_file(name: &str) -> Result<Vec<Line>> {
    let text = std::fs::read(data().join(name))?;
    let mut out = Vec::new();
    for (n, line) in text.split(|&b| b == b'\n').enumerate() {
        // Latin-1, as Perl reads a file without a layer.
        let line: String = line.iter().map(|&b| char::from(b)).collect();
        let line = line.trim_start();
        if line == "__END__" {
            break;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        read_line(line, &format!("{name}:{}", n.saturating_add(1)), &mut out)?;
    }
    Ok(out)
}

/// The text of a rectangle, as `vterm_screen_get_chars` reads it.
fn rect_chars(t: &mut dyn Term, [top, left, bottom, right]: [u16; 4]) -> Result<Vec<u32>> {
    let mut out = Vec::new();
    for row in top..bottom {
        let mut padding = 0usize;
        for col in left..right {
            let cell = t.cell(row, col)?;
            match cell.first() {
                None => padding = padding.saturating_add(1),
                Some(&CONTINUATION) => {}
                Some(_) => {
                    out.extend(std::iter::repeat_n(u32::from(' '), padding));
                    padding = 0;
                    out.extend(cell.iter().take(MAX_CHARS));
                }
            }
        }
        if row.saturating_add(1) < bottom {
            out.push(u32::from('\n'));
        }
    }
    Ok(out)
}

/// A `?screen_row` or `?screen_chars` expectation: a Perl string or a list
/// of codes. The harness compares `?screen_row` a byte at a time, each
/// character's code cut to its low byte; this compares whole codes, which
/// is stricter.
fn want_chars(want: &str) -> Result<Vec<u32>> {
    if want.starts_with('"') {
        perl(want)
    } else {
        hex_list(want)
    }
}

/// Codes as the harness writes them: `0x41,0x42`.
fn codes(chars: &[u32]) -> String {
    chars
        .iter()
        .map(|c| format!("0x{c:02x}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// What `t` gives for `check` at size `size`, and what the check wants,
/// each written alike.
fn observe(t: &mut dyn Term, check: &Check, (_, cols): (u16, u16)) -> Result<(String, String)> {
    let args = check.args.as_str();
    let want = check.want.as_str();
    let row_rect = |row: u16| [row, 0, row.saturating_add(1), cols];
    Ok(match check.kind.as_str() {
        "cursor" => {
            let (row, col) = t.cursor()?;
            (format!("{row},{col}"), want.to_owned())
        }
        "screen_row" => {
            let got = rect_chars(t, row_rect(args.parse()?))?;
            (codes(&got), codes(&want_chars(want)?))
        }
        "screen_chars" => {
            let rect = match numbers::<1>(args) {
                Ok([row]) => row_rect(row),
                Err(_) => numbers::<4>(args)?,
            };
            (codes(&rect_chars(t, rect)?), codes(&want_chars(want)?))
        }
        "screen_text" => {
            let text: String = rect_chars(t, numbers::<4>(args)?)?
                .into_iter()
                .map(|c| char::from_u32(c).ok_or("not a character"))
                .collect::<std::result::Result<_, _>>()?;
            let got: Vec<u32> = text.bytes().map(u32::from).collect();
            (codes(&got), codes(&hex_list(want)?))
        }
        "screen_eol" => {
            let [row, col] = numbers(args)?;
            let mut erased = true;
            for c in col..cols {
                erased &= t.cell(row, c)?.is_empty();
            }
            (u8::from(erased).to_string(), want.to_owned())
        }
        "screen_cell" => {
            let [row, col] = numbers(args)?;
            let (chars, width) = want
                .strip_prefix('{')
                .and_then(|w| w.split_once("} width="))
                .ok_or("a cell")?;
            let width = width.split(' ').next().ok_or("a width")?;
            let got: Vec<u32> = t.cell(row, col)?.into_iter().take(MAX_CHARS).collect();
            let next = col.saturating_add(1);
            let wide = next < cols && t.cell(row, next)?.first() == Some(&CONTINUATION);
            let got = format!("{{{}}} width={}", codes(&got), if wide { 2 } else { 1 });
            (
                got,
                format!("{{{}}} width={width}", codes(&hex_list(chars)?)),
            )
        }
        other => return Err(format!("no check {other}").into()),
    })
}

/// Runs every file on terminals `make` gives, one a file: the checks
/// failed, by `FILE:LINE`, each with what it wants and gets, and how many
/// ran.
fn run(
    make: &mut dyn FnMut() -> Result<Box<dyn Term>>,
) -> Result<(BTreeMap<String, String>, usize)> {
    let mut names: Vec<String> = std::fs::read_dir(data())?
        .map(|e| Ok(e?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_>>()?;
    names.retain(|n| n.ends_with(".test"));
    names.sort();
    let mut failed = BTreeMap::new();
    let mut count = 0usize;
    for name in names {
        let mut t = make()?;
        let mut size = (25, 80);
        for line in read_file(&name)? {
            match line {
                Line::Op(op) => {
                    if let Op::Resize(rows, cols) = op {
                        size = (rows, cols);
                    }
                    t.op(&op).map_err(|e| format!("{name}: {op:?}: {e}"))?;
                }
                Line::Check(check) => {
                    count = count.saturating_add(1);
                    let (got, want) = observe(t.as_mut(), &check, size)?;
                    if got != want {
                        let what =
                            format!("?{} {}: wants {want}, gets {got}", check.kind, check.args);
                        failed.insert(check.at, what);
                    }
                }
            }
        }
    }
    Ok((failed, count))
}

/// fux-vt, set up as the harness sets libvterm up.
struct Fux {
    parser: Option<Parser>,
    /// Whether `WANTSTATE` has reset it, as the harness's first does.
    state: bool,
    size: (u16, u16),
    reflow: bool,
    scrollback: bool,
    altscreen: bool,
    utf8: bool,
    /// What was written and resized since the last reset, to write again
    /// on a terminal set up anew.
    since_reset: Vec<Op>,
    /// The size at the last reset.
    reset_size: (u16, u16),
}

impl Fux {
    fn fresh(&self, (rows, cols): (u16, u16), history: usize) -> Result<Parser> {
        let options = Options::new().set(Feature::Reflow, self.reflow);
        Ok(Parser::with_options(
            Size::new(rows, cols)?,
            history,
            options,
        )?)
    }

    fn parser(&mut self) -> Result<&mut Parser> {
        Ok(self.parser.as_mut().ok_or("no INIT")?)
    }

    /// A new terminal with what was written since the last reset written
    /// again, after `before`. Resizes before anything was written make the
    /// new terminal that size.
    fn again(&mut self, history: usize, before: &[u8]) -> Result {
        let leading = self
            .since_reset
            .iter()
            .take_while(|op| matches!(op, Op::Resize(..)));
        let mut size = leading.clone().fold(self.reset_size, |size, op| {
            if let Op::Resize(rows, cols) = *op {
                (rows, cols)
            } else {
                size
            }
        });
        let mut p = self.fresh(size, history)?;
        p.process(before)?;
        for op in self.since_reset.iter().skip(leading.count()) {
            match *op {
                Op::Push(ref bytes) => p.process(bytes)?,
                Op::Resize(rows, cols) => {
                    // A taller screen would take back rows the harness
                    // never gave.
                    if history > 0 && rows > size.0 {
                        return Err("a taller screen written again with history".into());
                    }
                    size = (rows, cols);
                    p.resize(Size::new(rows, cols)?)?;
                }
                Op::Init | Op::State | Op::Screen(_) | Op::Utf8(_) | Op::Reset => {}
            }
        }
        self.parser = Some(p);
        Ok(())
    }
}

impl Term for Fux {
    fn op(&mut self, op: &Op) -> Result {
        match *op {
            Op::Init => {
                if self.parser.is_none() {
                    self.parser = Some(self.fresh(self.size, 0)?);
                }
            }
            Op::State if self.state => {}
            Op::State | Op::Reset => {
                self.state = true;
                self.reset_size = self.size;
                self.since_reset.clear();
                self.parser = Some(self.fresh(self.size, 0)?);
            }
            Op::Screen(ref flags) => {
                let mut sense = true;
                for flag in flags.chars() {
                    match flag {
                        '+' => sense = true,
                        '-' => sense = false,
                        'a' => self.altscreen = true,
                        'b' => {
                            self.scrollback = sense;
                            if !sense {
                                self.parser()?.set_history_bytes(0);
                            }
                        }
                        'r' if sense != self.reflow => {
                            self.reflow = sense;
                            self.again(0, b"")?;
                        }
                        _ => {}
                    }
                }
            }
            Op::Utf8(on) => self.utf8 = on,
            Op::Resize(rows, cols) => {
                if self.scrollback && rows > self.size.0 {
                    // Rows of `ABCDE` for the screen to take back: more
                    // than it grows by, each written in full, then the
                    // screen erased, the cursor home.
                    let extra = usize::from(rows).saturating_mul(2);
                    let mut seed = b"ABCDE".to_vec();
                    for _ in 0..extra {
                        seed.extend_from_slice(b"\r\nABCDE");
                    }
                    seed.extend_from_slice(b"\x1b[H\x1b[2J");
                    self.again(1 << 20, &seed)?;
                }
                self.size = (rows, cols);
                self.since_reset.push(op.clone());
                self.parser()?.resize(Size::new(rows, cols)?)?;
            }
            Op::Push(ref bytes) => {
                if !self.utf8 && bytes.iter().any(|&b| b >= 0x80) {
                    return Err("8-bit input, read as UTF-8".into());
                }
                let alternate = [&b"\x1b[?1049"[..], b"\x1b[?1047", b"\x1b[?47"];
                let has = |a: &[u8]| {
                    (0..bytes.len()).any(|i| bytes.get(i..).is_some_and(|s| s.starts_with(a)))
                };
                if !self.altscreen && alternate.iter().any(|a| has(a)) {
                    return Err("the alternate screen, not enabled".into());
                }
                self.since_reset.push(op.clone());
                self.parser()?.process(bytes)?;
            }
        }
        Ok(())
    }

    fn cursor(&mut self) -> Result<(u16, u16)> {
        Ok(self.parser()?.screen().cursor_position())
    }

    fn cell(&mut self, row: u16, col: u16) -> Result<Vec<u32>> {
        let screen = self.parser()?.screen();
        let cell = screen
            .cell(row, col)
            .ok_or_else(|| format!("no cell {row},{col}"))?;
        Ok(if cell.is_wide_continuation() {
            vec![CONTINUATION]
        } else {
            cell.contents().chars().map(u32::from).collect()
        })
    }
}

fn fux() -> Result<Box<dyn Term>> {
    Ok(Box::new(Fux {
        parser: None,
        state: false,
        size: (25, 80),
        reflow: false,
        scrollback: false,
        altscreen: false,
        utf8: false,
        since_reset: Vec::new(),
        reset_size: (25, 80),
    }))
}

/// libvterm's own `t/harness.c`, spoken to as `run-test.pl` speaks to it.
struct Harness {
    child: Child,
    to: ChildStdin,
    from: BufReader<ChildStdout>,
}

impl Harness {
    fn new(path: &str) -> Result<Self> {
        let mut child = Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let to = child.stdin.take().ok_or("no stdin")?;
        let from = BufReader::new(child.stdout.take().ok_or("no stdout")?);
        Ok(Self { child, to, from })
    }

    fn ask(&mut self, line: &str) -> Result<String> {
        writeln!(self.to, "{line}")?;
        self.to.flush()?;
        let mut answer = String::new();
        self.from.read_line(&mut answer)?;
        let answer = answer.trim();
        if answer.starts_with('!')
            || answer == "?"
            || answer.is_empty() && line.starts_with("?cursor")
        {
            return Err(format!("{line}: {answer}").into());
        }
        Ok(answer.to_owned())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Term for Harness {
    fn op(&mut self, op: &Op) -> Result {
        let line = match *op {
            Op::Init => "INIT".to_owned(),
            Op::State => "WANTSTATE".to_owned(),
            Op::Screen(ref flags) => format!("WANTSCREEN {flags}"),
            Op::Utf8(on) => format!("UTF8 {}", u8::from(on)),
            Op::Reset => "RESET".to_owned(),
            Op::Resize(rows, cols) => format!("RESIZE {rows}, {cols}"),
            Op::Push(ref bytes) => {
                let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                format!("PUSH {hex}")
            }
        };
        writeln!(self.to, "{line}")?;
        self.to.flush()?;
        // Callback lines, then DONE (or `?` for a line it did not take).
        loop {
            let mut answer = String::new();
            if self.from.read_line(&mut answer)? == 0 {
                return Err("the harness ended".into());
            }
            match answer.trim_end() {
                "DONE" => return Ok(()),
                "?" => return Err(format!("{line}: refused").into()),
                _ => {}
            }
        }
    }

    fn cursor(&mut self) -> Result<(u16, u16)> {
        Ok(numbers::<2>(&self.ask("?cursor")?)?.into())
    }

    fn cell(&mut self, row: u16, col: u16) -> Result<Vec<u32>> {
        let answer = self.ask(&format!("?screen_cell {row},{col}"))?;
        let chars = answer
            .strip_prefix('{')
            .and_then(|a| a.split_once('}'))
            .ok_or_else(|| format!("a cell: {answer}"))?
            .0;
        hex_list(chars)
    }
}

fn expected() -> Result<BTreeSet<String>> {
    let text = std::fs::read_to_string(data().join("expected.txt"))?;
    Ok(text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_owned)
        .collect())
}

/// How many of `failed` are outside the reflow file, of how many checks.
fn first_scope(failed: &BTreeMap<String, String>, count: usize) -> Result<(usize, usize)> {
    let reflow = read_file(REFLOW)?
        .iter()
        .filter(|l| matches!(l, Line::Check(_)))
        .count();
    let outside = failed.keys().filter(|f| !f.starts_with(REFLOW)).count();
    let total = count.saturating_sub(reflow);
    Ok((total.saturating_sub(outside), total))
}

#[test]
fn fux_vt_passes_libvterms_tests_but_the_listed() -> Result {
    let (failed, count) = run(&mut fux)?;
    let (pass, total) = first_scope(&failed, count)?;
    println!(
        "fux-vt passes {} of {count} ({pass} of the first {total})",
        count.saturating_sub(failed.len())
    );
    let expected = expected()?;
    let unexpected: Vec<_> = failed
        .iter()
        .filter(|(at, _)| !expected.contains(*at))
        .collect();
    let stale: Vec<_> = expected
        .iter()
        .filter(|at| !failed.contains_key(*at))
        .collect();
    assert!(
        unexpected.is_empty() && stale.is_empty(),
        "failed, not listed: {unexpected:?}; listed, passing: {stale:?}"
    );
    Ok(())
}

#[test]
#[ignore = "needs libvterm's harness, built from its source, in FUX_VT_LIBVTERM_HARNESS"]
fn libvterm_passes_its_own_tests() -> Result {
    let path = std::env::var("FUX_VT_LIBVTERM_HARNESS")?;
    let (failed, count) = run(&mut || Ok(Box::new(Harness::new(&path)?) as Box<dyn Term>))?;
    let (pass, total) = first_scope(&failed, count)?;
    println!(
        "libvterm passes {} of {count} ({pass} of the first {total})",
        count.saturating_sub(failed.len())
    );
    assert!(failed.is_empty(), "libvterm fails {failed:?}");
    Ok(())
}
