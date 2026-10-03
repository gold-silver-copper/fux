//! xterm.js, as @xterm/headless under Node, read into a snapshot.
//!
//! One Node process per thread serves every terminal: `node/engine.mjs`,
//! started when the first terminal is made, and fed one JSON request per
//! line on its stdin, each answered by one line on its stdout. Output goes
//! in base64; an answered write is answered once xterm.js's write callback
//! has fired, so the bytes are parsed and applied. `feed` sends a workload
//! in writes of up to a MiB, answering only the last. `node/package.json`
//! and its lockfile pin @xterm/headless 6.0.0 and
//! @xterm/addon-unicode-graphemes 0.4.0; `run.sh` installs them.
//!
//! How it is set up, and read:
//!
//! - 100000 rows of history, far more than fux-vt keeps.
//! - The graphemes addon, so widths are measured by grapheme cluster from
//!   Unicode 15, as fux-vt measures them and Ghostty does with mode 2027.
//!   Without it xterm.js measures code points by Unicode 6, where most
//!   emoji are narrow.
//! - `reflowCursorLine`, so the cursor's line is reflowed on resize as in
//!   fux-vt and Ghostty. Off (the default), xterm.js cuts it at the new
//!   width and leaves the rest to a shell to redraw: `replay --size 3x8
//!   'abcdefgh12' resize:3x4` would lose `efgh`.
//! - Cells through the public buffer API: `getChars` (a cluster whole),
//!   `getWidth` (0 for the second half of a wide glyph), the colour modes
//!   and the attribute flags. A wide glyph that did not fit leaves an
//!   ordinary blank at the end of its row. A cell with a hyperlink (OSC 8)
//!   reads as underlined through `isUnderline`, whatever its SGR, as
//!   xterm.js draws links with a dashed underline; its SGR underline is
//!   then the core's own flag in `fg`, which SGR 4, 24 and 0 set and
//!   clear (`replay --engines xterm.js --size 1x3 '\e]8;;u\e\\ab'`).
//! - xterm.js keeps a row's soft-wrap flag on the row that continues it
//!   (`isWrapped`), so row y is wrapped when row y + 1 is; the newest
//!   history row is wrapped when the top screen row is.
//! - A cursor waiting to wrap is one past the last column (`cursorX` is
//!   the width), so that is read as pending wrap in the last column.
//! - Modes from `term.modes`; the alternate screen from the active
//!   buffer's type; the title from `onTitleChange`; replies from `onData`.
//!   Cursor visibility is not in the public API: it is the core's own flag,
//!   `coreService.isCursorHidden`.
//!
//! - Hyperlinks are not in the public API: a cell's ExtendedAttrs hold its
//!   link's number (`urlId`), and the core's OscLinkService the URI
//!   (`getLinkData`). It numbers each OSC 8 without an id anew, and one
//!   with an id and a URI it has seen as before, so the number tells links
//!   apart.
//!
//! What it cannot tell: underline colour, which the public API does not
//! give. The core keeps one only on an underlined cell (it drops `58` on
//! plain text) and reads SGR 59 back as white, not as no colour. Kitty
//! keyboard flags: xterm.js 6.0 does not implement the protocol. In-band
//! resize and prompt marks, which it does not implement either.
//!
//! Its quirks, beside fux-vt where Ghostty agrees with fux-vt, as found by
//! `run`, `matrix` and the named cases:
//!
//! - It is never narrower than two columns: asked for one, it has two, and
//!   says so (`replay --size 1x1`). So it refuses a terminal or a resize
//!   narrower than that, and abstains from the case.
//! - A cursor report while a wrap is pending gives the column one past the
//!   last (`replay --size 1x5 'abcde\e[6n'`: `CSI 1;6R`).
//! - Erasing a whole row that continues another (EL 2, ED, or ED 1 over
//!   it) unmarks the row before it as soft-wrapped, as the flag lives on
//!   the continuation (`replay --size 2x3 'abcdef\e[2;1H\e[2K'`); the same
//!   unwraps the newest history row when the top screen row is erased.
//! - DECSC and DECRC keep the cursor's row in the buffer, history
//!   included, so a restore after the screen scrolls lands as many rows
//!   higher (`replay --size 3x3 '\e[2;1H\e7\n\n\n\e8X'`); and they do not
//!   keep a pending wrap (`replay --size 1x3 'abc\e7\e8'`).
//! - With origin mode on, relative cursor movement adds the top margin
//!   twice (`replay --size 4x2 '\e[2;4r\e[?6h\e[1B'`: row 3, not 2).
//! - DECSTR also resets DECAWM, DECCKM, focus reporting, DECTCEM and SGR,
//!   all of which fux-vt and Ghostty keep
//!   (`replay --size 2x4 '\e[?1h\e[?7l\e[1mA\e[!pB'`).
//! - RIS leaves a hidden cursor hidden (`replay --size 2x4 '\e[?25l\ec'`).
//! - RI ends a pending wrap (`replay --size 1x3 'abc\eM'`).
//! - SGR `38:2:1:2:3` takes 1 as the colour space, so the colour is
//!   (2, 3, 0) (`replay --size 1x2 '\e[38:2:1:2:3md'`).
//! - A combining mark with nothing before it takes a cell of its own
//!   (`replay --size 2x2 '\u{301}'`).
//! - Reflow moves the cursor's row with its text, not its column
//!   (`replay --size 1x2 --history 10000 --newline-before-resize '-li'
//!   resize:3x10`: column 0, not 2).
//! - ED 1 with the cursor in the last column clears the soft-wrap flag of
//!   buffer line y + 1, not counting history: on the last row before there
//!   is any history, there is no such line, and xterm.js 6.0.0 throws
//!   before erasing the rows above, and its write never ends.
//!   `engine.mjs` gives it a stand-in line to clear there, so it does all
//!   it means to (`replay --size 2x3 'xyzabc\e[1J'`).
use crate::engine::{Can, Engine, Kind, Setup};
use crate::snapshot::{self, Cell, Color, Line, Snapshot, Style, Width};
use serde_json::Value;
use std::cell::RefCell;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// The most bytes in one write while feeding a workload.
const PIECE: usize = 1 << 20;

/// On request only: behind a process of its own, it is slower than the
/// engines in this process.
pub const KIND: Kind = Kind {
    name: "xterm.js",
    about: "xterm.js, as @xterm/headless under Node",
    can: Can {
        underline_color: false,
        kitty_keyboard_flags: false,
        in_band_resize: false,
        prompt: false,
        ..Can::ALL
    },
    panel: false,
    in_process: false,
    available,
    make,
};

/// The directory of the Node package: `node/` beside this crate's manifest.
fn package() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("node")
}

fn available() -> Result<(), String> {
    let version = Command::new("node")
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("no node: {e}"))?;
    if !version.status.success() {
        return Err("node --version failed".into());
    }
    let modules = package().join("node_modules").join("@xterm");
    for name in ["headless", "addon-unicode-graphemes"] {
        if !modules.join(name).is_dir() {
            return Err(format!(
                "@xterm/{name} is not installed in {}: run run.sh, which installs it",
                package().display()
            ));
        }
    }
    Ok(())
}

/// The Node process serving every terminal of this thread, started when
/// the first is made.
struct Node {
    child: Child,
    input: BufWriter<ChildStdin>,
    output: BufReader<ChildStdout>,
    next: u64,
}

thread_local! {
    static NODE: RefCell<Option<Node>> = const { RefCell::new(None) };
}

impl Node {
    fn start() -> Result<Node, String> {
        let script = package().join("engine.mjs");
        let mut child = Command::new("node")
            .arg(&script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("starting node {}: {e}", script.display()))?;
        let input = child.stdin.take().ok_or("node: no stdin")?;
        let output = child.stdout.take().ok_or("node: no stdout")?;
        Ok(Node {
            child,
            input: BufWriter::with_capacity(1 << 16, input),
            output: BufReader::new(output),
            next: 0,
        })
    }

    /// Sends a request that is not answered.
    fn send(&mut self, line: &str) -> Result<(), String> {
        self.input
            .write_all(line.as_bytes())
            .and_then(|()| self.input.write_all(b"\n"))
            .map_err(|e| format!("writing to node: {e}"))
    }

    /// Sends a request and reads its answer.
    fn ask(&mut self, line: &str) -> Result<Value, String> {
        self.send(line)?;
        self.input
            .flush()
            .map_err(|e| format!("writing to node: {e}"))?;
        let mut answer = String::new();
        let read = self
            .output
            .read_line(&mut answer)
            .map_err(|e| format!("reading from node: {e}"))?;
        if read == 0 {
            return Err("node exited".into());
        }
        let value: Value =
            serde_json::from_str(&answer).map_err(|e| format!("node's answer: {e}"))?;
        if value.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(value)
        } else {
            Err(value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("no error given")
                .to_owned())
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Runs `f` on this thread's Node process, starting it if need be. A
/// process that has exited is started again for the next terminal.
fn with_node<T>(f: impl FnOnce(&mut Node) -> Result<T, String>) -> Result<T, String> {
    NODE.try_with(|cell| {
        let mut slot = cell
            .try_borrow_mut()
            .map_err(|e| format!("node in use: {e}"))?;
        if slot
            .as_mut()
            .is_some_and(|node| !matches!(node.child.try_wait(), Ok(None)))
        {
            *slot = None;
        }
        if slot.is_none() {
            *slot = Some(Node::start()?);
        }
        let node = slot.as_mut().ok_or("node did not start")?;
        let result = f(node);
        drop(slot);
        result
    })
    .map_err(|e| format!("node: {e}"))?
}

/// `bytes` in base64, as engine.mjs reads it.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    let mut put = |n: u32, sextets: usize| {
        for (i, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            let letter = if i < sextets {
                usize::try_from(n.wrapping_shr(shift) & 63)
                    .ok()
                    .and_then(|at| ALPHABET.get(at))
                    .map_or('=', |&b| char::from(b))
            } else {
                '='
            };
            out.push(letter);
        }
    };
    let (whole, rest) = bytes.as_chunks::<3>();
    for &[a, b, c] in whole {
        put(u32::from_be_bytes([0, a, b, c]), 4);
    }
    match *rest {
        [a] => put(u32::from_be_bytes([0, a, 0, 0]), 2),
        [a, b] => put(u32::from_be_bytes([0, a, b, 0]), 3),
        _ => {}
    }
    out
}

pub struct XtermJs {
    id: u64,
}

/// xterm.js is never narrower than two columns (it reports its real
/// size), so it abstains from a narrower terminal rather than differ on
/// its size.
const MIN_COLS: u16 = 2;

fn make(setup: &Setup) -> Result<Box<dyn Engine>, String> {
    if setup.cols < MIN_COLS {
        return Err(format!("never narrower than {MIN_COLS} columns"));
    }
    let id = with_node(|node| {
        let id = node.next;
        node.next = id.checked_add(1).ok_or("too many terminals")?;
        node.ask(&format!(
            r#"{{"op":"create","id":{id},"rows":{},"cols":{}}}"#,
            setup.rows, setup.cols
        ))?;
        Ok(id)
    })?;
    Ok(Box::new(XtermJs { id }))
}

impl Drop for XtermJs {
    fn drop(&mut self) {
        let id = self.id;
        let _ = with_node(|node| node.send(&format!(r#"{{"op":"dispose","id":{id}}}"#)));
    }
}

fn get<'a>(v: &'a Value, key: &str) -> Result<&'a Value, String> {
    v.get(key).ok_or_else(|| format!("snapshot: no {key}"))
}

fn flag(v: &Value, key: &str) -> Result<bool, String> {
    get(v, key)?
        .as_bool()
        .ok_or_else(|| format!("snapshot: {key} is not a boolean"))
}

fn number(v: &Value, key: &str) -> Result<u16, String> {
    get(v, key)?
        .as_u64()
        .and_then(|n| u16::try_from(n).ok())
        .ok_or_else(|| format!("snapshot: {key} is not a size"))
}

fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    get(v, key)?
        .as_str()
        .ok_or_else(|| format!("snapshot: {key} is not a string"))
}

/// A colour as engine.mjs gives it: -1 the default, 0–255 a palette entry,
/// 0x1000000 plus the RGB value a direct colour.
fn color(v: Option<&Value>) -> Result<Color, String> {
    let n = v
        .and_then(Value::as_i64)
        .ok_or("snapshot: a colour is not a number")?;
    if n < 0 {
        return Ok(Color::Default);
    }
    if let Ok(index) = u8::try_from(n) {
        return Ok(Color::Idx(index));
    }
    let rgb = n
        .checked_sub(0x100_0000)
        .and_then(|rgb| u32::try_from(rgb).ok())
        .ok_or("snapshot: a colour is out of range")?;
    let [_, r, g, b] = rgb.to_be_bytes();
    Ok(Color::Rgb(r, g, b))
}

/// A cell as engine.mjs gives it: `[text, width, fg, bg, flags, link]`,
/// its width xterm.js's own (0 for the second half of a wide glyph), its
/// link `[uri, number]` or null.
fn cell(v: &Value) -> Result<Cell, String> {
    let fields = v.as_array().ok_or("snapshot: a cell is not an array")?;
    let at = |i: usize| fields.get(i);
    let text = at(0)
        .and_then(Value::as_str)
        .ok_or("snapshot: a cell has no text")?;
    let width = match at(1).and_then(Value::as_u64) {
        Some(0) => Width::Tail,
        Some(2) => Width::Wide,
        Some(_) => Width::Narrow,
        None => return Err("snapshot: a cell has no width".into()),
    };
    let flags = at(4)
        .and_then(Value::as_u64)
        .ok_or("snapshot: a cell has no flags")?;
    let on = |bit: u64| flags & bit != 0;
    let style = Style {
        fg: color(at(2))?,
        bg: color(at(3))?,
        underline_color: Color::Default,
        bold: on(1),
        dim: on(2),
        italic: on(4),
        underline: on(8),
        blink: on(16),
        inverse: on(32),
        hidden: on(64),
        strikeout: on(128),
    };
    let link = match at(5) {
        None | Some(Value::Null) => None,
        Some(v) => {
            let uri = v.get(0).and_then(Value::as_str);
            let number = v.get(1).and_then(Value::as_u64);
            let (Some(uri), Some(number)) = (uri, number) else {
                return Err("snapshot: a cell's link is not [uri, number]".into());
            };
            Some((uri.to_owned(), number.to_string()))
        }
    };
    Ok(Cell::new(text, width, style).linked(link))
}

fn line(v: &Value) -> Result<Line, String> {
    let cells = get(v, "c")?
        .as_array()
        .ok_or("snapshot: a row's cells are not an array")?
        .iter()
        .map(cell)
        .collect::<Result<_, _>>()?;
    Ok(Line {
        unread_from: None,
        prompt: false,
        cells,
        wrapped: flag(v, "w")?,
    })
}

fn history_row(v: &Value) -> Result<(String, bool), String> {
    let text = v
        .get(0)
        .and_then(Value::as_str)
        .ok_or("snapshot: a history row has no text")?;
    let wrapped = v
        .get(1)
        .and_then(Value::as_bool)
        .ok_or("snapshot: a history row has no wrap flag")?;
    Ok((text.to_owned(), wrapped))
}

fn read(v: &Value) -> Result<Snapshot, String> {
    let rows = number(v, "rows")?;
    let cols = number(v, "cols")?;
    // xterm.js keeps a cursor waiting to wrap one past the last column.
    let x = number(v, "x")?;
    let last = cols.saturating_sub(1);
    let list = |key: &str| -> Result<&Vec<Value>, String> {
        get(v, key)?
            .as_array()
            .ok_or_else(|| format!("snapshot: {key} is not an array"))
    };
    Ok(Snapshot {
        rows,
        cols,
        cursor: (number(v, "y")?, x.min(last)),
        pending_wrap: x > last,
        cursor_visible: !flag(v, "hidden")?,
        autowrap: flag(v, "autowrap")?,
        origin: flag(v, "origin")?,
        alternate: flag(v, "alternate")?,
        application_cursor: flag(v, "application_cursor")?,
        application_keypad: flag(v, "application_keypad")?,
        bracketed_paste: flag(v, "bracketed_paste")?,
        synchronized_output: flag(v, "synchronized_output")?,
        in_band_resize: false,
        focus_reporting: flag(v, "focus_reporting")?,
        kitty_keyboard_flags: 0,
        title: text(v, "title")?.to_owned(),
        reports: snapshot::reports(text(v, "replies")?.as_bytes()),
        screen: list("screen")?.iter().map(line).collect::<Result<_, _>>()?,
        history: list("history")?
            .iter()
            .map(history_row)
            .collect::<Result<_, _>>()?,
    })
}

impl XtermJs {
    fn write(&self, node: &mut Node, bytes: &[u8], ack: bool) -> Result<(), String> {
        let line = format!(
            r#"{{"op":"write","id":{},"data":"{}","ack":{ack}}}"#,
            self.id,
            base64(bytes)
        );
        if ack {
            node.ask(&line).map(drop)
        } else {
            node.send(&line)
        }
    }
}

impl Engine for XtermJs {
    fn process(&mut self, bytes: &[u8]) -> Result<(), String> {
        with_node(|node| self.write(node, bytes, true))
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        if cols < MIN_COLS {
            return Err(format!("never narrower than {MIN_COLS} columns"));
        }
        let id = self.id;
        with_node(|node| {
            node.ask(&format!(
                r#"{{"op":"resize","id":{id},"rows":{rows},"cols":{cols}}}"#
            ))
            .map(drop)
        })
    }

    fn snapshot(&mut self, history_rows: usize) -> Result<Snapshot, String> {
        let id = self.id;
        let answer = with_node(|node| {
            node.ask(&format!(
                r#"{{"op":"snapshot","id":{id},"history":{history_rows}}}"#
            ))
        })?;
        read(&answer)
    }

    /// The whole workload in writes of up to a MiB, none answered but the
    /// last: Node applies them in order, so its answer means all are done.
    fn feed(&mut self, bytes: &[u8], _: usize) -> Result<(), String> {
        with_node(|node| {
            let mut rest = bytes;
            while !rest.is_empty() {
                let (now, later) = rest
                    .split_at_checked(PIECE.min(rest.len()))
                    .unwrap_or((rest, &[]));
                self.write(node, now, later.is_empty())?;
                rest = later;
            }
            Ok(())
        })
    }
}
