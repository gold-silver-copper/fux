//! What the engines behind a process of their own share, tmux and xterm:
//! the pane program that runs inside the terminal, the sync that waits for
//! the terminal to catch up, and reading rows of text and SGR sequences
//! back into cells.
//!
//! # The pane program
//!
//! A terminal runs one program on its pty. Here it is a shell line:
//!
//! ```sh
//! stty raw -echo -opost ...; exec 3<&0; cat <&3 >OUT & exec cat <IN
//! ```
//!
//! With the tty raw and output processing off, no byte is translated on
//! the way through (with `onlcr`, LF would reach the terminal as CR LF).
//! The harness writes its bytes into the FIFO `IN`, and `cat` copies them
//! to the terminal unchanged. What the terminal answers arrives on the
//! tty's input; the other `cat` copies it into the FIFO `OUT`, which a
//! thread here reads. A background job's input is `/dev/null` in a
//! non-interactive shell, so the tty is handed to it through fd 3. When
//! the harness closes `IN`, or dies, `cat` reaches the end of its input
//! and the pane exits, and the terminal with it.
//!
//! The FIFOs and every other file live in
//! `std::env::temp_dir()/fux-vt-compare-<pid>/`, and are removed when
//! their engine is dropped (the directory too, once it is empty).
//!
//! # Sync
//!
//! After the bytes, the harness asks for primary device attributes
//! (`CSI c`) and waits for the reply (`CSI ? ... c`). A terminal answers
//! in order, so once it has arrived every byte before it has been
//! processed. DA1 and not a cursor position report: cases ask for cursor
//! reports themselves, and none asks for device attributes. Real programs
//! do (delta, tmux, Claude Code, in the corpus), so every DA1 request
//! written, the output's and the harness's, is counted ([`Requests`]), and
//! a sync waits for as many replies. Every DA1 reply is taken out of the
//! replies kept (they are not reports: `snapshot::reports` drops them).
//! Every other reply to the case's output is kept, and filtered by
//! `snapshot::reports`.
//!
//! Quirks of syncing in band:
//!
//! - A sequence left unfinished at the end of a step is cut short by the
//!   request, where an engine in this process would carry it into the next
//!   step's bytes.
//! - Inside a string left open (DCS, OSC, SOS, PM, APC), the request is
//!   swallowed. After two seconds without a reply the harness sends CAN
//!   and ST, which end any string, and asks again.
//!
//! # Reading rows
//!
//! Both terminals print a row as its text with SGR sequences before each
//! change of style (tmux's `capture-pane -e`, xterm's media copy with
//! `printAttributes: 2`). [`Reader`] turns such a row back into cells,
//! carrying the style from row to row as both print it. How a run of
//! characters splits into cells, and how wide each is, differs; see
//! [`Glyphs`].
use crate::snapshot::{self, Cell, Color, Style, Width};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

/// The request every sync sends: primary device attributes.
const SYNC: &[u8] = b"\x1b[c";

/// What ends any string the request could be lost in: CAN, then ST.
const UNSTICK: &[u8] = b"\x18\x1b\\";

/// How long to wait for a reply before asking again, and then in all.
const FIRST_WAIT: Duration = Duration::from_secs(2);
const LAST_WAIT: Duration = Duration::from_secs(10);

/// While a sync waits with something to do meanwhile, the longest it
/// waits for a reply before doing it again.
const TURN: Duration = Duration::from_millis(1);

/// What a sync does while it waits, if anything: a multiplexer's client
/// must be read, and its queries answered, for the multiplexer to go on.
type Meanwhile<'a> = Option<&'a mut dyn FnMut() -> Result<(), String>>;

/// This process's directory for FIFOs and files, made if it is missing.
pub fn dir() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("fux-vt-compare-{}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir)
}

/// Removes this process's directory if nothing is left in it.
pub fn tidy() {
    let dir = std::env::temp_dir().join(format!("fux-vt-compare-{}", std::process::id()));
    let _ = fs::remove_dir(dir);
}

/// A number no other call in this process gets: for names of files,
/// sessions and the like.
pub fn serial() -> usize {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A new path in this process's directory.
pub fn fresh(stem: &str) -> Result<PathBuf, String> {
    Ok(dir()?.join(format!("{stem}-{}", serial())))
}

/// `text` quoted for `sh`.
pub fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Whether `program` is on `PATH`.
pub fn on_path(program: &str) -> Result<(), String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    if std::env::split_paths(&path).any(|dir| dir.join(program).is_file()) {
        Ok(())
    } else {
        Err(format!("{program} is not on PATH"))
    }
}

fn mkfifo(path: &Path) -> Result<(), String> {
    let status = Command::new("mkfifo")
        .arg(path)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("mkfifo: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("mkfifo {}: {status}", path.display()))
    }
}

/// Opens a FIFO for reading and writing at once, which never waits for the
/// other end, and closes it again: whoever waits to open it at the other
/// end stops waiting.
fn unstick(path: &Path) {
    let _ = OpenOptions::new().read(true).write(true).open(path);
}

/// What the terminal has answered, as a thread here reads it.
#[derive(Default)]
struct Heard {
    bytes: Vec<u8>,
    /// The pane has closed its end: the terminal is gone.
    closed: bool,
}

#[derive(Default)]
struct Inbox {
    heard: Mutex<Heard>,
    arrived: Condvar,
}

fn listen(path: &Path, inbox: &Inbox) {
    let mut buf = vec![0u8; 1 << 16];
    if let Ok(mut file) = File::open(path) {
        while let Ok(n) = file.read(&mut buf) {
            if n == 0 {
                break;
            }
            if let (Ok(mut heard), Some(got)) = (inbox.heard.lock(), buf.get(..n)) {
                heard.bytes.extend_from_slice(got);
            }
            inbox.arrived.notify_all();
        }
    }
    if let Ok(mut heard) = inbox.heard.lock() {
        heard.closed = true;
    }
    inbox.arrived.notify_all();
}

/// Where the replies to `CSI c` are in `bytes`: `ESC [ ? digits;... c`.
fn da_replies(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(start) = bytes
        .get(at..)
        .and_then(|rest| rest.iter().position(|&b| b == 0x1b))
        .and_then(|i| i.checked_add(at))
    {
        let body = start.saturating_add(3);
        let is_da = bytes.get(start..body) == Some(b"\x1b[?".as_slice());
        let end = bytes
            .get(body..)
            .and_then(|rest| {
                rest.iter()
                    .position(|b| !(b.is_ascii_digit() || *b == b';'))
            })
            .and_then(|i| i.checked_add(body));
        match end {
            Some(end) if is_da && bytes.get(end) == Some(&b'c') => {
                let after = end.saturating_add(1);
                out.push((start, after));
                at = after;
            }
            _ => at = start.saturating_add(1),
        }
    }
    out
}

/// Where a scan of written bytes is: a DA1 request counts only outside
/// strings, and only whole.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Scan {
    #[default]
    Ground,
    Escape,
    /// Inside `CSI`: whether it can still be DA1 (`CSI c`, `CSI 0 c`).
    Csi(bool),
    /// Inside a string; OSC's ends at BEL too.
    String {
        osc: bool,
    },
    /// ESC inside a string: ST if `\` follows.
    StringEscape,
}

/// Counts the DA1 requests in bytes written to a terminal, as it reads
/// them, across writes: each gets a reply the sync must wait for.
#[derive(Clone, Copy, Debug, Default)]
pub struct Requests {
    scan: Scan,
}

impl Requests {
    /// How many DA1 requests end in `bytes`.
    pub fn count(&mut self, bytes: &[u8]) -> usize {
        let mut found = 0usize;
        for &b in bytes {
            self.scan = match (self.scan, b) {
                // CAN and SUB end any sequence or string.
                (_, 0x18 | 0x1a) => Scan::Ground,
                (Scan::String { osc: true }, 0x07) => Scan::Ground,
                (Scan::String { .. }, 0x1b) => Scan::StringEscape,
                (Scan::String { osc }, _) => Scan::String { osc },
                (Scan::StringEscape, b'\\') => Scan::Ground,
                (_, 0x1b) => Scan::Escape,
                (Scan::Escape | Scan::StringEscape, b'[') => Scan::Csi(true),
                (Scan::Escape | Scan::StringEscape, b']') => Scan::String { osc: true },
                (Scan::Escape | Scan::StringEscape, b'P' | b'X' | b'^' | b'_') => {
                    Scan::String { osc: false }
                }
                (Scan::Escape | Scan::StringEscape, 0x20..=0x2f) => Scan::Escape,
                (Scan::Escape | Scan::StringEscape, _) => Scan::Ground,
                (Scan::Csi(da), b'0') => Scan::Csi(da),
                (Scan::Csi(_), 0x20..=0x3f) => Scan::Csi(false),
                (Scan::Csi(da), 0x40..=0x7e) => {
                    if da && b == b'c' {
                        found = found.saturating_add(1);
                    }
                    Scan::Ground
                }
                // Other controls are carried out inside a sequence.
                (Scan::Csi(da), _) => Scan::Csi(da),
                (Scan::Ground, _) => Scan::Ground,
            };
        }
        found
    }
}

/// The pipe to a terminal through its pane program.
pub struct Pane {
    input: Option<File>,
    opening: Option<mpsc::Receiver<std::io::Result<File>>>,
    inbox: Arc<Inbox>,
    /// Where the replies not yet taken by a sync start.
    mark: usize,
    /// The replies to the output given so far, without the syncs'.
    kept: Vec<u8>,
    /// DA1 requests written whose replies no sync has taken yet.
    requests: Requests,
    asked: usize,
    fifos: [PathBuf; 2],
}

impl Pane {
    /// Makes the FIFOs and starts listening. Gives the pane, and the shell
    /// line the terminal runs as its program.
    pub fn new() -> Result<(Pane, String), String> {
        let into = fresh("in")?;
        let from = fresh("out")?;
        mkfifo(&into)?;
        mkfifo(&from)?;
        let inbox = Arc::new(Inbox::default());
        let listener = Arc::clone(&inbox);
        let path = from.clone();
        thread::Builder::new()
            .name("pane-replies".into())
            .spawn(move || listen(&path, &listener))
            .map_err(|e| format!("thread: {e}"))?;
        // Opening a FIFO to write waits for its reader: the pane program,
        // once the terminal runs it.
        let (tx, rx) = mpsc::channel();
        let path = into.clone();
        thread::Builder::new()
            .name("pane-open".into())
            .spawn(move || {
                let _ = tx.send(OpenOptions::new().write(true).open(&path));
            })
            .map_err(|e| format!("thread: {e}"))?;
        let program = format!(
            "stty raw -echo -opost -icrnl -ixon -isig -icanon -iexten 2>/dev/null; \
             exec 3<&0; cat <&3 >{} & exec cat <{}",
            quote(&from.to_string_lossy()),
            quote(&into.to_string_lossy())
        );
        Ok((
            Pane {
                input: None,
                opening: Some(rx),
                inbox,
                mark: 0,
                kept: Vec::new(),
                requests: Requests::default(),
                asked: 0,
                fifos: [into, from],
            },
            program,
        ))
    }

    /// Waits for the pane program to start, at most `limit`: whether it
    /// has.
    pub fn connect(&mut self, limit: Duration) -> Result<bool, String> {
        let rx = self
            .opening
            .as_ref()
            .ok_or("the pane is already connected")?;
        match rx.recv_timeout(limit) {
            Ok(Ok(file)) => {
                self.input = Some(file);
                self.opening = None;
                Ok(true)
            }
            Ok(Err(e)) => Err(format!("opening the pane's input: {e}")),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(false),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err("the pane's opener is gone".into()),
        }
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.asked = self.asked.saturating_add(self.requests.count(bytes));
        self.input
            .as_mut()
            .ok_or("the pane is not connected")?
            .write_all(bytes)
            .map_err(|e| format!("writing to the pane: {e}"))
    }

    /// Waits until a DA1 reply to every DA1 request written has arrived
    /// after `mark` (one, at least), or `limit` has passed, doing
    /// `meanwhile` every [`TURN`] if there is one. Gives the replies up to
    /// the last DA1 reply, with every DA1 reply taken out, and moves
    /// `mark` past them.
    fn wait(
        &mut self,
        limit: Duration,
        meanwhile: &mut Meanwhile<'_>,
    ) -> Result<Option<Vec<u8>>, String> {
        let deadline = Instant::now()
            .checked_add(limit)
            .ok_or("a deadline out of range")?;
        let inbox = Arc::clone(&self.inbox);
        let lock = || {
            inbox
                .heard
                .lock()
                .map_err(|_| "the reply buffer is poisoned")
        };
        let mut heard = lock()?;
        loop {
            let fresh = heard.bytes.get(self.mark..).unwrap_or_default();
            let found = da_replies(fresh);
            if let Some(&(_, end)) = found.last()
                && found.len() >= self.asked
            {
                self.asked = 0;
                let mut batch = Vec::new();
                let mut at = 0usize;
                for &(start, after) in &found {
                    batch.extend_from_slice(fresh.get(at..start).unwrap_or_default());
                    at = after;
                }
                self.mark = self.mark.saturating_add(end);
                drop(heard);
                return Ok(Some(batch));
            }
            if heard.closed {
                return Err("the terminal is gone".into());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            let turn = if meanwhile.is_some() {
                left.min(TURN)
            } else {
                left
            };
            heard = inbox
                .arrived
                .wait_timeout(heard, turn)
                .map_err(|_| "the reply buffer is poisoned")?
                .0;
            if let Some(meanwhile) = meanwhile.as_mut() {
                drop(heard);
                meanwhile()?;
                heard = lock()?;
            }
        }
    }

    /// Asks for device attributes and waits for the reply: everything
    /// written before has been processed. Gives the replies since the last
    /// sync, without the syncs'.
    pub fn sync(&mut self) -> Result<Vec<u8>, String> {
        self.sync_while(None)
    }

    fn sync_while(&mut self, mut meanwhile: Meanwhile<'_>) -> Result<Vec<u8>, String> {
        self.write(SYNC)?;
        if let Some(batch) = self.wait(FIRST_WAIT, &mut meanwhile)? {
            return Ok(batch);
        }
        let mut again = UNSTICK.to_vec();
        again.extend_from_slice(SYNC);
        self.write(&again)?;
        self.wait(LAST_WAIT, &mut meanwhile)?
            .ok_or_else(|| "no reply to a device attributes request".to_owned())
    }

    /// Gives the terminal a program's output and waits for it to be
    /// processed, keeping its replies.
    pub fn output(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.output_while(bytes, None)
    }

    /// [`Pane::output`], doing `meanwhile` while it waits, if anything.
    pub fn output_while(&mut self, bytes: &[u8], meanwhile: Meanwhile<'_>) -> Result<(), String> {
        self.write(bytes)?;
        let replies = self.sync_while(meanwhile)?;
        self.kept.extend_from_slice(&replies);
        Ok(())
    }

    /// Gives the terminal the harness's own requests and gives their
    /// replies, which are not kept.
    pub fn ask(&mut self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        self.write(bytes)?;
        self.sync()
    }

    /// The cursor position and status reports given to the output so far.
    pub fn reports(&self) -> Vec<String> {
        snapshot::reports(&self.kept)
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        // The pane program sees the end of its input and exits.
        self.input = None;
        // Threads still waiting to open a FIFO stop waiting.
        for fifo in &self.fifos {
            unstick(fifo);
            let _ = fs::remove_file(fifo);
        }
        tidy();
    }
}

/// The value of the mode `mode` in a DECRQM reply (`CSI ? mode ; value $ y`)
/// in `bytes`: 1 set, 2 reset, 0 unknown.
pub fn mode(bytes: &[u8], mode: u16) -> Option<u8> {
    let head = format!("\x1b[?{mode};");
    let text = String::from_utf8_lossy(bytes);
    let (_, rest) = text.split_once(&head)?;
    let (value, _) = rest.split_once("$y")?;
    value.parse().ok()
}

/// How a terminal's printed row splits into cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyphs {
    /// tmux prints a cell's whole cluster as tmux joined it, and not its
    /// width, which is worked out as tmux works it out
    /// (`screen_write_combine`): a zero-width character, one after a ZWJ,
    /// a skin tone after one of the emoji it modifies, and a second
    /// regional indicator join the cell before; a cell is wide if its first
    /// character is, or if it holds VS16, a skin tone or two regional
    /// indicators. Tabs print as one `\t` for the blank cells they span.
    /// Cells drawn from the DEC special graphics set print between SO and
    /// SI as the ASCII character they were given, and read as tmux draws
    /// them.
    Tmux,
    /// xterm prints a cell's character, then the combining characters it
    /// holds, and the second half of a wide character as U+FFFF (with
    /// `printRawChars`), so widths are xterm's own.
    ///
    /// Its SGR before a cell is whole (`0;…`), and is printed before every
    /// cell with an attribute or a colour, and before no other cell after
    /// one: `printLine` compares each cell with no attributes, not with the
    /// cell before (`IAttr attr = 0` inside its loop, xterm 411 `print.c`).
    /// So `\e[7ma\e[mb` prints as `\e[0;7mab`, and a cell with no SGR
    /// before it is plain, whatever came before.
    Xterm,
}

/// A piece of a printed row.
enum Piece {
    /// A glyph, its style and its hyperlink's URI.
    Glyph(String, Style, Option<String>),
    /// The second half of a wide glyph (xterm).
    Tail,
    /// Blank cells (a tmux tab).
    Blanks(usize, Style, Option<String>),
}

fn is_modifier(c: char) -> bool {
    ('\u{1f3fb}'..='\u{1f3ff}').contains(&c)
}

fn is_regional(c: char) -> bool {
    ('\u{1f1e6}'..='\u{1f1ff}').contains(&c)
}

/// The emoji tmux lets a skin tone modify (`utf8_should_combine`).
fn takes_modifier(c: char) -> bool {
    matches!(
        u32::from(c),
        0x1f44b..=0x1f450
            | 0x1f466..=0x1f469
            | 0x1f46e
            | 0x1f470..=0x1f478
            | 0x1f47c
            | 0x1f481..=0x1f483
            | 0x1f485..=0x1f487
            | 0x1f4aa
            | 0x1f575
            | 0x1f57a
            | 0x1f590
            | 0x1f595
            | 0x1f596
            | 0x1f645..=0x1f647
            | 0x1f64b..=0x1f64f
            | 0x1f6b4..=0x1f6b6
            | 0x1f926
            | 0x1f937..=0x1f939
            | 0x1f93d
            | 0x1f93e
            | 0x1f9b5
            | 0x1f9b6
            | 0x1f9b8
            | 0x1f9b9
            | 0x1f9cd..=0x1f9cf
            | 0x1f9d1..=0x1f9df
    )
}

/// The width tmux gives a cell holding `cluster`.
fn tmux_width(cluster: &str) -> usize {
    let mut chars = cluster.chars();
    let first = chars.next().and_then(UnicodeWidthChar::width).unwrap_or(1);
    if first >= 2 || chars.any(|c| c == '\u{fe0f}' || is_modifier(c) || is_regional(c)) {
        2
    } else {
        1
    }
}

/// Whether `c` joins the cell holding `prev`.
fn joins(prev: &str, c: char, glyphs: Glyphs) -> bool {
    if prev.is_empty() {
        return false;
    }
    if c.width() == Some(0) {
        return true;
    }
    match glyphs {
        Glyphs::Xterm => false,
        Glyphs::Tmux => {
            let mut chars = prev.chars();
            let first = chars.next();
            let single = chars.next().is_none();
            prev.ends_with('\u{200d}')
                || (is_modifier(c) && first.is_some_and(takes_modifier))
                || (is_regional(c) && single && first.is_some_and(is_regional))
        }
    }
}

/// A character of the DEC special graphics set as Unicode, as tmux and
/// xterm draw it.
fn dec_graphics(c: char) -> char {
    match c {
        '_' => '\u{a0}',
        '`' => '◆',
        'a' => '▒',
        'b' => '␉',
        'c' => '␌',
        'd' => '␍',
        'e' => '␊',
        'f' => '°',
        'g' => '±',
        'h' => '␤',
        'i' => '␋',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        other => other,
    }
}

/// A palette index from text, if it is one.
fn index(text: Option<&&str>) -> Option<Color> {
    text.and_then(|t| t.parse().ok()).map(Color::Idx)
}

/// An RGB colour from the last three of `parts`.
fn rgb(parts: &[&str]) -> Option<Color> {
    let n = parts.len();
    let at = |back: usize| -> Option<u8> { parts.get(n.checked_sub(back)?)?.parse().ok() };
    Some(Color::Rgb(at(3)?, at(2)?, at(1)?))
}

/// Applies an SGR sequence's parameters to `style`: the sequences tmux and
/// xterm print, `;` between parameters and `:` within one. tmux prints the
/// attributes numbered from 10 up with a colon: `4:2` to `4:5` are
/// underline styles, and `5:3` is overline (53), not blink.
pub fn sgr(params: &str, style: &mut Style) {
    let groups: Vec<Vec<&str>> = params.split(';').map(|g| g.split(':').collect()).collect();
    let mut i = 0usize;
    while let Some(group) = groups.get(i) {
        i = i.saturating_add(1);
        let code: u16 = group.first().and_then(|c| c.parse().ok()).unwrap_or(0);
        let sub = group.get(1);
        match code {
            0 => *style = Style::default(),
            1 => style.bold = true,
            2 => style.dim = true,
            3 => style.italic = true,
            4 => style.underline = sub.is_none_or(|s| *s != "0"),
            5 if sub == Some(&"3") => {}
            5 | 6 => style.blink = true,
            7 => style.inverse = true,
            8 => style.hidden = true,
            9 => style.strikeout = true,
            21 => style.underline = true,
            22 => {
                style.bold = false;
                style.dim = false;
            }
            23 => style.italic = false,
            24 => style.underline = false,
            25 => style.blink = false,
            27 => style.inverse = false,
            28 => style.hidden = false,
            29 => style.strikeout = false,
            30..=37 => style.fg = Color::Idx(low(code.saturating_sub(30))),
            39 => style.fg = Color::Default,
            40..=47 => style.bg = Color::Idx(low(code.saturating_sub(40))),
            49 => style.bg = Color::Default,
            59 => style.underline_color = Color::Default,
            90..=97 => style.fg = Color::Idx(low(code.saturating_sub(82))),
            100..=107 => style.bg = Color::Idx(low(code.saturating_sub(92))),
            38 | 48 | 58 => {
                let color = if group.len() > 1 {
                    match sub.copied() {
                        Some("5") => index(group.get(2)),
                        Some("2") => rgb(group.get(2..).unwrap_or_default()),
                        _ => None,
                    }
                } else {
                    let kind = groups.get(i).and_then(|g| g.first()).copied();
                    let (color, used) = match kind {
                        Some("5") => (
                            index(groups.get(i.saturating_add(1)).and_then(|g| g.first())),
                            2,
                        ),
                        Some("2") => {
                            let parts: Vec<&str> = (1..4)
                                .filter_map(|k| {
                                    groups
                                        .get(i.saturating_add(k))
                                        .and_then(|g| g.first())
                                        .copied()
                                })
                                .collect();
                            (rgb(&parts).filter(|_| parts.len() == 3), 4)
                        }
                        _ => (None, 0),
                    };
                    i = i.saturating_add(used);
                    color
                };
                if let Some(color) = color {
                    match code {
                        38 => style.fg = color,
                        48 => style.bg = color,
                        _ => style.underline_color = color,
                    }
                }
            }
            _ => {}
        }
    }
}

/// A small code as a palette index.
fn low(n: u16) -> u8 {
    u8::try_from(n).unwrap_or(0)
}

/// Reads printed rows back into cells, keeping the style (and the
/// hyperlink) from row to row.
pub struct Reader {
    /// How many cells the last row printed, before padding.
    pub printed: usize,
    style: Style,
    /// The URI of the hyperlink an `OSC 8` in the print opened, until
    /// one closes it (tmux's `-e`).
    link: Option<String>,
    shifted: bool,
    glyphs: Glyphs,
}

impl Reader {
    pub fn new(glyphs: Glyphs) -> Reader {
        Reader {
            printed: 0,
            style: Style::default(),
            link: None,
            shifted: false,
            glyphs,
        }
    }

    /// After a cell, xterm's next cell is plain unless an SGR comes first
    /// (see [`Glyphs::Xterm`]).
    fn unstyle(&mut self) {
        if self.glyphs == Glyphs::Xterm {
            self.style = Style::default();
        }
    }

    fn width(&self, piece: &Piece) -> usize {
        match piece {
            Piece::Glyph(text, ..) => match self.glyphs {
                Glyphs::Tmux => tmux_width(text),
                Glyphs::Xterm => 1,
            },
            Piece::Tail => 1,
            Piece::Blanks(n, ..) => *n,
        }
    }

    /// One printed row as `cols` cells, padded with blanks. `tab` gives how
    /// many cells a tab spans from a column (tmux).
    pub fn row(&mut self, text: &[u8], cols: u16, tab: &dyn Fn(usize) -> usize) -> Vec<Cell> {
        let text = String::from_utf8_lossy(text);
        let mut pieces: Vec<Piece> = Vec::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\x1b' => match chars.next() {
                    Some('[') => {
                        let mut params = String::new();
                        let mut last = None;
                        for d in chars.by_ref() {
                            if ('\x40'..='\x7e').contains(&d) {
                                last = Some(d);
                                break;
                            }
                            params.push(d);
                        }
                        if last == Some('m') {
                            sgr(&params, &mut self.style);
                        }
                    }
                    Some(']') => {
                        let mut body = String::new();
                        while let Some(d) = chars.next() {
                            if d == '\x07' || (d == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                                break;
                            }
                            body.push(d);
                        }
                        // OSC 8 ; params ; URI: an empty URI closes the link.
                        if let Some((_, uri)) =
                            body.strip_prefix("8;").and_then(|r| r.split_once(';'))
                        {
                            self.link = (!uri.is_empty()).then(|| uri.to_owned());
                        }
                    }
                    Some('#' | '(' | ')' | '*' | '+') => {
                        chars.next();
                    }
                    _ => {}
                },
                '\x0e' => self.shifted = true,
                '\x0f' => self.shifted = false,
                '\t' => {
                    let at = pieces.iter().map(|p| self.width(p)).sum();
                    pieces.push(Piece::Blanks(tab(at).max(1), self.style, self.link.clone()));
                }
                '\u{ffff}' => {
                    pieces.push(Piece::Tail);
                    self.unstyle();
                }
                c if c.is_control() => {}
                c => {
                    let c = if self.shifted { dec_graphics(c) } else { c };
                    match pieces.last_mut() {
                        Some(Piece::Glyph(prev, ..)) if joins(prev, c, self.glyphs) => prev.push(c),
                        _ => {
                            pieces.push(Piece::Glyph(c.to_string(), self.style, self.link.clone()));
                            self.unstyle();
                        }
                    }
                }
            }
        }
        let cols = usize::from(cols);
        let mut cells: Vec<Cell> = Vec::with_capacity(cols);
        for piece in &pieces {
            match piece {
                Piece::Glyph(text, style, link) => {
                    let wide = self.width(piece) == 2;
                    cells.push(
                        Cell::new(text, if wide { Width::Wide } else { Width::Narrow }, *style)
                            .linked(link.clone().map(|uri| (uri, String::new()))),
                    );
                    if wide {
                        cells.push(Cell::new("", Width::Tail, Style::default()));
                    }
                }
                Piece::Tail => {
                    if let Some(last) = cells.last_mut()
                        && last.width == Width::Narrow
                    {
                        last.width = Width::Wide;
                    }
                    cells.push(Cell::new("", Width::Tail, Style::default()));
                }
                Piece::Blanks(n, style, link) => {
                    for _ in 0..*n {
                        cells.push(
                            Cell::new("", Width::Narrow, *style)
                                .linked(link.clone().map(|uri| (uri, String::new()))),
                        );
                    }
                }
            }
        }
        cells.truncate(cols);
        self.printed = cells.len();
        while cells.len() < cols {
            cells.push(Cell::new("", Width::Narrow, Style::default()));
        }
        cells
    }
}

/// Where a short-lived directory with sockets goes: `$TMPDIR` when it is set
/// and short, else `~/.cache`. A socket's path must stay under 104 bytes, and
/// macOS's `$TMPDIR` takes half of that; `/tmp` is memory on some machines.
pub fn short_temp_dir() -> Result<std::path::PathBuf, String> {
    let tmp = std::env::temp_dir();
    if std::env::var_os("TMPDIR").is_some_and(|v| !v.is_empty()) && tmp.as_os_str().len() <= 40 {
        return tmp
            .canonicalize()
            .map_err(|e| format!("{}: {e}", tmp.display()));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let dir = std::path::PathBuf::from(home).join(".cache");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    dir.canonicalize()
        .map_err(|e| format!("{}: {e}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_attribute_requests_are_counted_outside_strings() {
        let mut r = Requests::default();
        assert_eq!(r.count(b"\x1b[c\x1b[0c\x1b[>c\x1b[?1c\x1b[1c"), 2);
        // Across writes.
        assert_eq!(r.count(b"ab\x1b["), 0);
        assert_eq!(r.count(b"c"), 1);
        // Not inside a string; after one ends by BEL (OSC alone), ST or
        // CAN.
        assert_eq!(r.count(b"\x1b]11;?\x07\x1b[c"), 1);
        assert_eq!(r.count(b"\x1bP+q[c\x1b\\\x1b[c"), 1);
        assert_eq!(r.count(b"\x1b_G\x07[c\x1b\\"), 0);
        assert_eq!(r.count(b"\x1b_G\x18\x1b[c"), 1);
        // ESC inside a string, not ST, ends it and starts a sequence.
        assert_eq!(r.count(b"\x1b]2;t\x1b[c"), 1);
    }

    #[test]
    fn device_attribute_replies_are_found() {
        let bytes = b"\x1b[1;2R\x1b[?64;1;2c\x1b[0n\x1b[?1;2;4c";
        assert_eq!(da_replies(bytes), [(6, 16), (20, 29)]);
    }

    #[test]
    fn sgr_reads_both_forms() {
        let mut style = Style::default();
        sgr("1;38;5;100;48:2::1:2:3;4:3;5:3", &mut style);
        assert!(style.bold && style.underline && !style.blink);
        assert_eq!(style.fg, Color::Idx(100));
        assert_eq!(style.bg, Color::Rgb(1, 2, 3));
        sgr("0;91;58;2;4;5;6", &mut style);
        assert_eq!(style.fg, Color::Idx(9));
        assert_eq!(style.underline_color, Color::Rgb(4, 5, 6));
    }

    /// tmux's `-e` prints a hyperlink as OSC 8 before its cells and closes
    /// it after; a link left open goes on into the next row.
    #[test]
    fn hyperlinks_are_read_from_osc_8() {
        let mut tmux = Reader::new(Glyphs::Tmux);
        let uris = |cells: &[Cell]| -> Vec<Option<String>> {
            cells
                .iter()
                .map(|c| c.link.as_ref().map(|l| l.uri.clone()))
                .collect()
        };
        let cells = tmux.row(
            "a\x1b]8;;http://a\x1b\\b\x1b]8;;\x1b\\c\x1b]8;id=1;http://b\x07\t".as_bytes(),
            6,
            &|_| 2,
        );
        let a = Some("http://a".to_owned());
        let b = Some("http://b".to_owned());
        assert_eq!(uris(&cells), [None, a, None, b.clone(), b.clone(), None]);
        let cells = tmux.row(b"d", 2, &|_| 1);
        assert_eq!(uris(&cells), [b, None]);
    }

    #[test]
    fn rows_split_into_cells() {
        let mut tmux = Reader::new(Glyphs::Tmux);
        let cells = tmux.row("a界e\u{301}❤\u{fe0f}\t\x0eq\x0f".as_bytes(), 10, &|_| 2);
        let text: Vec<&str> = cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            text,
            ["a", "界", "", "e\u{301}", "❤\u{fe0f}", "", "", "", "─", ""]
        );
        let mut xterm = Reader::new(Glyphs::Xterm);
        let cells = xterm.row("a界\u{ffff}b".as_bytes(), 5, &|_| 1);
        let widths: Vec<Width> = cells.iter().map(|c| c.width).collect();
        assert_eq!(
            widths,
            [
                Width::Narrow,
                Width::Wide,
                Width::Tail,
                Width::Narrow,
                Width::Narrow
            ]
        );
    }

    /// xterm prints an SGR before every cell with an attribute, and none
    /// before a plain cell that follows one.
    #[test]
    fn an_xterm_cell_with_no_sgr_before_it_is_plain() {
        let mut xterm = Reader::new(Glyphs::Xterm);
        let cells = xterm.row(
            "\x1b[0m\x1b[0;7ma\x1b[0;7me\u{301}x\x1b[0;44m \x1b[0my".as_bytes(),
            5,
            &|_| 1,
        );
        let read: Vec<(&str, bool, Color)> = cells
            .iter()
            .map(|c| (c.text.as_str(), c.style.inverse, c.style.bg))
            .collect();
        assert_eq!(
            read,
            [
                ("a", true, Color::Default),
                ("e\u{301}", true, Color::Default),
                ("x", false, Color::Default),
                ("", false, Color::Idx(4)),
                ("y", false, Color::Default),
            ]
        );
    }
}
