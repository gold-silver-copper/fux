//! A search index over terminal recordings: each recording is replayed
//! through fux-vt, each distinct screen it showed is kept with its time,
//! and each word's stays on screen are indexed by where they were.
pub mod json;

use fux_vt::{Mode, Options, Parser, RowId, Size};
use std::collections::HashMap;
use std::rc::Rc;

/// Where in a recording: seconds for a cast, bytes for a raw log.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub enum At {
    Secs(f64),
    Byte(u64),
}

impl std::fmt::Display for At {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            At::Secs(s) => write!(f, "{s:.3}s"),
            At::Byte(b) => write!(f, "byte {b}"),
        }
    }
}

/// A word on screen for a while: in which recording, from when until when,
/// at which row and column it came (it may scroll on from there), and its
/// row's text then.
#[derive(Clone, Debug)]
pub struct Hit {
    pub file: usize,
    pub from: At,
    pub until: At,
    pub row: u16,
    pub col: u16,
    pub line: Rc<str>,
}

/// A screen as it was from `at` on: its rows' text, shared with the
/// screens before it where a row did not change.
pub struct State {
    pub at: At,
    pub rows: Vec<Rc<str>>,
}

#[derive(Default)]
pub struct Index {
    pub files: Vec<String>,
    /// Each recording's distinct screens, in order.
    pub states: Vec<Vec<State>>,
    /// Each word, lowercased, and its stays on screen.
    words: HashMap<String, Vec<Hit>>,
}

/// The words of a row's text, each with the column it starts at.
fn words(cells: &[(u16, char)]) -> Vec<(u16, String)> {
    let mut found = Vec::new();
    let mut current: Option<(u16, String)> = None;
    for &(col, c) in cells {
        if c.is_alphanumeric() || c == '_' {
            current
                .get_or_insert_with(|| (col, String::new()))
                .1
                .extend(c.to_lowercase());
        } else if let Some(word) = current.take() {
            found.push(word);
        }
    }
    found.extend(current);
    found
}

/// A screen row as last read: which row it is, at which version, its
/// text, its trailing blanks cut, and its words with their columns.
#[derive(Clone)]
struct Line {
    id: RowId,
    version: u64,
    text: Rc<str>,
    words: Vec<(u16, String)>,
}

impl Line {
    fn read(row: fux_vt::Row<'_>) -> Line {
        let mut cells = Vec::new();
        let mut text = String::new();
        for (col, cell) in row.glyphs() {
            let shown = cell.shown();
            text.push_str(shown);
            cells.extend(shown.chars().map(|c| (col, c)));
        }
        Line {
            id: row.id(),
            version: row.version(),
            text: Rc::from(text.trim_end()),
            words: words(&cells),
        }
    }
}

/// One recording being replayed into the index.
struct Replay<'a> {
    index: &'a mut Index,
    file: usize,
    parser: Parser,
    /// The screen's rows as last read.
    rows: Vec<Line>,
    /// The words on screen now, by the row that holds them (kept as it
    /// scrolls), their column and themselves: their hit in the index.
    open: HashMap<(RowId, u16, String), usize>,
}

impl Replay<'_> {
    /// Reads every byte of `bytes`, replies to the program's queries going
    /// nowhere: it is long gone. A refusal of room loses what it was for
    /// and nothing else, so it is not an error here.
    fn feed(&mut self, bytes: &[u8]) {
        let _ = self.parser.process_all(bytes, &mut ());
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        let size = Size::new(rows, cols).map_err(|e| e.to_string())?;
        self.parser.resize(size).map_err(|e| e.to_string())
    }

    /// Records the screen as it is at `at`, if it differs from the last.
    fn observe(&mut self, at: At) {
        let screen = self.parser.screen();
        // Mid-frame, the program has not finished drawing.
        if screen.mode(Mode::SynchronizedOutput) {
            return;
        }
        let before: HashMap<RowId, Line> = std::mem::take(&mut self.rows)
            .into_iter()
            .map(|l| (l.id, l))
            .collect();
        let window = screen.window();
        // A row of the same identity and version is unchanged: only the
        // rows edited since are read again.
        let rows: Vec<Line> = window
            .iter()
            .map(|(_, row)| match before.get(&row.id()) {
                Some(old) if old.version == row.version() => old.clone(),
                _ => Line::read(row),
            })
            .collect();
        let mut changed = rows.len() != before.len();
        for (r, line) in rows.iter().enumerate() {
            let r = u16::try_from(r).unwrap_or(u16::MAX);
            match before.get(&line.id) {
                Some(old) if old.text == line.text => {}
                old => {
                    changed = true;
                    self.reword(
                        line.id,
                        r,
                        old.map_or(&[][..], |o| &o.words),
                        &line.words,
                        &line.text,
                        at,
                    );
                }
            }
        }
        for old in before.values() {
            if !rows.iter().any(|line| line.id == old.id) {
                changed = true;
                self.reword(old.id, 0, &old.words, &[], "", at);
            }
        }
        if changed && let Some(states) = self.index.states.get_mut(self.file) {
            states.push(State {
                at,
                rows: rows.iter().map(|line| line.text.clone()).collect(),
            });
        }
        self.rows = rows;
    }

    /// Closes the stays of the words row `id` (now at screen row `row`) no
    /// longer has, and opens those it now has.
    fn reword(
        &mut self,
        id: RowId,
        row: u16,
        before: &[(u16, String)],
        now: &[(u16, String)],
        text: &str,
        at: At,
    ) {
        for (col, word) in before {
            if !now.contains(&(*col, word.clone())) {
                self.close(id, *col, word.clone(), at);
            }
        }
        let line: Rc<str> = Rc::from(text);
        for (col, word) in now {
            if !before.contains(&(*col, word.clone())) {
                let hits = self.index.words.entry(word.clone()).or_default();
                self.open.insert((id, *col, word.clone()), hits.len());
                // Its line is the row's text as it was when the word came.
                hits.push(Hit {
                    file: self.file,
                    from: at,
                    until: at,
                    row,
                    col: *col,
                    line: line.clone(),
                });
            }
        }
    }

    /// Ends the stay of `word` at `col` of row `id` at `at`.
    fn close(&mut self, id: RowId, col: u16, word: String, at: At) {
        let Some(hit) = self.open.remove(&(id, col, word.clone())) else {
            return;
        };
        if let Some(hit) = self
            .index
            .words
            .get_mut(&word)
            .and_then(|hits| hits.get_mut(hit))
        {
            hit.until = at;
        }
    }

    /// Ends every stay at the recording's end.
    fn finish(mut self, at: At) {
        let open: Vec<_> = self.open.keys().cloned().collect();
        for (id, col, word) in open {
            self.close(id, col, word, at);
        }
    }
}

/// What a recording holds: a cast's size and timed output, or raw bytes.
pub enum Recording {
    /// The screen's size, and each event: when, and its output or resize.
    Cast {
        rows: u16,
        cols: u16,
        events: Vec<(f64, Event)>,
    },
    Raw(Vec<u8>),
}

pub enum Event {
    Output(String),
    Resize { rows: u16, cols: u16 },
}

/// "COLSxROWS", as a cast's resize event gives it.
fn cast_size(text: &str) -> Option<(u16, u16)> {
    let (cols, rows) = text.split_once('x')?;
    Some((rows.trim().parse().ok()?, cols.trim().parse().ok()?))
}

impl Recording {
    /// An asciicast (v2 or v3) if its first line is one's header, else raw
    /// bytes.
    pub fn read(bytes: Vec<u8>) -> Result<Recording, String> {
        let first = bytes.split(|b| *b == b'\n').next().unwrap_or_default();
        let header = std::str::from_utf8(first)
            .ok()
            .and_then(|line| json::parse(line).ok());
        let Some(header) = header.filter(|h| h.get("version").is_some()) else {
            return Ok(Recording::Raw(bytes));
        };
        let version = header.get("version").and_then(json::Json::num);
        let term = header.get("term");
        let dim = |v2: &str, v3: &str| {
            (header.get(v2).or_else(|| term.and_then(|t| t.get(v3))))
                .and_then(json::Json::num)
                .and_then(|n| u16::try_from(n as i64).ok())
                .ok_or_else(|| format!("a cast header with no {v2}"))
        };
        let (rows, cols) = (dim("height", "rows")?, dim("width", "cols")?);
        let relative = match version {
            Some(2.0) => false,
            Some(3.0) => true,
            _ => return Err("an asciicast version other than 2 or 3".into()),
        };
        let text = String::from_utf8_lossy(&bytes);
        let mut events = Vec::new();
        let mut time = 0.0;
        for (n, line) in text.lines().enumerate().skip(1) {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let event =
                json::parse(line).map_err(|e| format!("line {}: {e}", n.saturating_add(1)))?;
            let json::Json::Arr(fields) = event else {
                return Err(format!("line {}: not an event", n.saturating_add(1)));
            };
            let (Some(t), Some(code), Some(data)) = (
                fields.first().and_then(json::Json::num),
                fields.get(1).and_then(json::Json::str),
                fields.get(2).and_then(json::Json::str),
            ) else {
                return Err(format!(
                    "line {}: not [time, code, data]",
                    n.saturating_add(1)
                ));
            };
            time = if relative { time + t } else { t };
            match code {
                "o" => events.push((time, Event::Output(data.to_owned()))),
                "r" => {
                    if let Some((rows, cols)) = cast_size(data) {
                        events.push((time, Event::Resize { rows, cols }));
                    }
                }
                _ => {}
            }
        }
        Ok(Recording::Cast { rows, cols, events })
    }
}

/// The most a raw log is read at once between looks at the screen.
const RAW_CHUNK: usize = 4096;

impl Index {
    /// Replays `recording`, named `name`, into the index; a raw log on a
    /// screen of `size` (rows, columns).
    pub fn add(
        &mut self,
        name: &str,
        recording: &Recording,
        size: (u16, u16),
    ) -> Result<(), String> {
        let (rows, cols) = match recording {
            Recording::Cast { rows, cols, .. } => (*rows, *cols),
            Recording::Raw(_) => size,
        };
        let screen = Size::new(rows, cols).map_err(|e| format!("{name}: {e}"))?;
        let parser = Parser::with_options(screen, 0, Options::default())
            .map_err(|e| format!("{name}: {e}"))?;
        let file = self.files.len();
        self.files.push(name.to_owned());
        self.states.push(Vec::new());
        let mut replay = Replay {
            index: self,
            file,
            parser,
            rows: Vec::new(),
            open: HashMap::new(),
        };
        match recording {
            Recording::Cast { events, .. } => {
                replay.observe(At::Secs(0.0));
                let mut end = 0.0;
                for (time, event) in events {
                    match event {
                        Event::Output(text) => replay.feed(text.as_bytes()),
                        Event::Resize { rows, cols } => replay
                            .resize(*rows, *cols)
                            .map_err(|e| format!("{name}: {e}"))?,
                    }
                    replay.observe(At::Secs(*time));
                    end = *time;
                }
                replay.finish(At::Secs(end));
            }
            Recording::Raw(bytes) => {
                replay.observe(At::Byte(0));
                let mut at = 0usize;
                while at < bytes.len() {
                    let rest = bytes.get(at..).unwrap_or_default();
                    let window = rest.get(..RAW_CHUNK).unwrap_or(rest);
                    // Up to and with the next line feed, if one is near.
                    let len = window
                        .iter()
                        .position(|b| *b == b'\n')
                        .map_or(window.len(), |n| n.saturating_add(1));
                    replay.feed(rest.get(..len).unwrap_or_default());
                    at = at.saturating_add(len);
                    replay.observe(At::Byte(at as u64));
                }
                replay.finish(At::Byte(bytes.len() as u64));
            }
        }
        Ok(())
    }

    /// Every stay on screen of `word`, any case; a trailing `*` matches
    /// every word it starts. In order of recording, then time.
    pub fn search(&self, word: &str) -> Vec<&Hit> {
        let word = word.to_lowercase();
        let mut hits: Vec<&Hit> = match word.strip_suffix('*') {
            Some(prefix) => (self.words.iter())
                .filter(|(w, _)| w.starts_with(prefix))
                .flat_map(|(_, hits)| hits)
                .collect(),
            None => self.words.get(&word).into_iter().flatten().collect(),
        };
        hits.sort_by(|a, b| {
            (a.file.cmp(&b.file))
                .then(
                    a.from
                        .partial_cmp(&b.from)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
                .then((a.row, a.col).cmp(&(b.row, b.col)))
        });
        hits
    }

    /// The screen recording `file` showed at `at`.
    pub fn screen_at(&self, file: usize, at: At) -> Option<&State> {
        let states = self.states.get(file)?;
        states.iter().take_while(|s| s.at <= at).last()
    }

    pub fn word_count(&self) -> usize {
        self.words.len()
    }
}
