//! A test oracle for TUIs: replay what a program wrote to its terminal
//! through fux-vt, then check assertions about the screen it left.
//!
//! Assertions, one a line (`#` starts a comment), rows and columns counted
//! from 0:
//!
//! ```text
//! cell ROW COL TEXT      the cells from COL show TEXT, a grapheme a cell
//! cursor ROW COL         the cursor is there
//! mode NAME on|off       a mode: fux_vt::Mode's name (ShowCursor, show-cursor)
//!                        or its number (?25 for DECSET's, 4 for SM's)
//! title TEXT             the last title OSC 0 or 2 set
//! fg ROW COL COLOR       the cell's foreground: default, 0-255, a name
//!                        (red, bright-blue), or #rrggbb
//! ```
//!
//! TEXT is the rest of the line, or a `"`-quoted string with `\"`, `\\`,
//! `\t` and `\e` escapes, to keep blanks at its ends.
use fux_vt::{Color, Feature, Mode, Parser, Rgb, Size};
use std::fmt;

/// One thing to check of the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Assertion {
    Cell { row: u16, col: u16, text: String },
    Cursor { row: u16, col: u16 },
    Mode { mode: Mode, on: bool },
    Title(String),
    Fg { row: u16, col: u16, color: Color },
}

/// An assertion and the line of the file it came from, counted from 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub number: usize,
    pub source: String,
    pub assertion: Assertion,
}

/// Every assertion of `text`, or the first line that isn't one.
pub fn parse(text: &str) -> Result<Vec<Line>, String> {
    let mut lines = Vec::new();
    for (index, source) in text.lines().enumerate() {
        let number = index.saturating_add(1);
        let trimmed = source.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let assertion = assertion(trimmed).map_err(|e| format!("line {number}: {e}: {source}"))?;
        lines.push(Line {
            number,
            source: trimmed.trim_end().to_owned(),
            assertion,
        });
    }
    Ok(lines)
}

/// The first word of `text` and the rest after the blanks that follow it.
fn word(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    let (word, rest) = text.split_once([' ', '\t']).unwrap_or((text, ""));
    (word, rest.trim_start())
}

fn number(text: &str, what: &str) -> Result<u16, String> {
    text.parse().map_err(|_| format!("{what} {text:?} is not a number"))
}

fn assertion(line: &str) -> Result<Assertion, String> {
    let (kind, rest) = word(line);
    let (first, after_first) = word(rest);
    let (second, after_second) = word(after_first);
    Ok(match kind {
        "cell" => Assertion::Cell {
            row: number(first, "row")?,
            col: number(second, "column")?,
            text: text(after_second)?,
        },
        "cursor" => Assertion::Cursor {
            row: number(first, "row")?,
            col: number(second, "column")?,
        },
        "mode" => Assertion::Mode {
            mode: mode(first)?,
            on: match second {
                "on" => true,
                "off" => false,
                _ => return Err(format!("a mode is on or off, not {second:?}")),
            },
        },
        "title" => Assertion::Title(text(rest)?),
        "fg" => Assertion::Fg {
            row: number(first, "row")?,
            col: number(second, "column")?,
            color: color(after_second.trim_end())?,
        },
        _ => return Err(format!("no assertion {kind:?}")),
    })
}

/// The rest of a line as text: quoted, with escapes, or as it stands.
fn text(rest: &str) -> Result<String, String> {
    let rest = rest.trim_end();
    let Some(quoted) = rest.strip_prefix('"') else {
        return Ok(rest.to_owned());
    };
    let mut out = String::new();
    let mut chars = quoted.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' if chars.as_str().is_empty() => return Ok(out),
            '"' => return Err("text after the closing quote".into()),
            '\\' => out.push(match chars.next() {
                Some('t') => '\t',
                Some('e') => '\x1b',
                Some(c @ ('"' | '\\')) => c,
                other => return Err(format!("no escape \\{}", other.unwrap_or(' '))),
            }),
            c => out.push(c),
        }
    }
    Err("no closing quote".into())
}

/// The mode `name` names: a `Mode`'s name, any case, `-` and `_` ignored, or
/// its number, `?` first for a DEC private mode.
pub fn mode(name: &str) -> Result<Mode, String> {
    let key = |s: &str| s.chars().filter(|c| !matches!(c, '-' | '_')).collect::<String>();
    let wanted = key(name).to_ascii_lowercase();
    let by_number = match name.strip_prefix('?') {
        Some(n) => n.parse().ok().map(|n| (true, n)),
        None => name.parse().ok().map(|n| (false, n)),
    };
    Mode::ALL
        .into_iter()
        .find(|&mode| match by_number {
            Some(number) => mode.number() == number,
            None => format!("{mode:?}").to_ascii_lowercase() == wanted,
        })
        .ok_or_else(|| format!("no mode {name:?}"))
}

const NAMES: [&str; 8] = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];

pub fn color(text: &str) -> Result<Color, String> {
    if text == "default" {
        return Ok(Color::Default);
    }
    if let Ok(index) = text.parse::<u8>() {
        return Ok(Color::Idx(index));
    }
    if let Some(hex) = text.strip_prefix('#')
        && hex.len() == 6
        && let Ok(rgb) = u32::from_str_radix(hex, 16)
    {
        let [_, r, g, b] = rgb.to_be_bytes();
        return Ok(Color::Rgb(Rgb { r, g, b }));
    }
    let (bright, name) = match text.strip_prefix("bright-") {
        Some(name) => (8, name),
        None => (0, text),
    };
    let index = NAMES.iter().position(|&n| n == name);
    let index = index.and_then(|i| u8::try_from(i).ok());
    index
        .map(|i| Color::Idx(i.saturating_add(bright)))
        .ok_or_else(|| format!("no colour {text:?}: default, 0-255, a name or #rrggbb"))
}

/// A colour as an assertion writes it.
pub struct Shown(pub Color);

impl fmt::Display for Shown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Color::Default => f.write_str("default"),
            Color::Idx(i) => match NAMES.get(usize::from(i % 8)) {
                Some(name) if i < 8 => write!(f, "{i} ({name})"),
                Some(name) if i < 16 => write!(f, "{i} (bright-{name})"),
                _ => write!(f, "{i}"),
            },
            Color::Rgb(Rgb { r, g, b }) => write!(f, "#{r:02x}{g:02x}{b:02x}"),
        }
    }
}

/// A screen a recording left.
pub struct Replay {
    parser: Parser,
}

impl Replay {
    /// Every byte of `recording` read into a terminal of `size`.
    pub fn new(recording: &[u8], size: Size) -> Result<Replay, fux_vt::Error> {
        // Events for titles, and the modes a feature must turn on to be seen.
        let options = [Feature::Events, Feature::ColorSchemeUpdates, Feature::InBandResize];
        let mut parser = Parser::with_options(size, 0, options.into_iter().collect())?;
        let _ = parser.process_all(recording, &mut ());
        Ok(Replay { parser })
    }

    /// The last title the program set; `None` if none was (or it was set
    /// empty: the screen does not tell those apart).
    fn title(&self) -> Option<String> {
        let title = self.parser.screen().title().bytes;
        (!title.is_empty()).then(|| String::from_utf8_lossy(title).into_owned())
    }

    /// Whether `assertion` holds: what the screen shows instead if not.
    pub fn check(&self, assertion: &Assertion) -> Result<(), String> {
        let screen = self.parser.screen();
        match assertion {
            Assertion::Cell { row, col, text } => {
                let want: Vec<&str> = fux_vt::clusters(text).map(|(c, _)| c).collect();
                let got = self.cells(*row, *col, want.len())?;
                match got == want {
                    true => Ok(()),
                    false => Err(format!("{:?}", got.concat())),
                }
            }
            Assertion::Cursor { row, col } => match screen.cursor_position() {
                (r, c) if (r, c) == (*row, *col) => Ok(()),
                (r, c) => Err(format!("cursor {r} {c}")),
            },
            Assertion::Mode { mode, on } => match screen.mode(*mode) {
                set if set == *on => Ok(()),
                set => Err(format!("{mode:?} is {}", if set { "on" } else { "off" })),
            },
            Assertion::Title(want) => match &self.title() {
                Some(title) if title == want => Ok(()),
                Some(title) => Err(format!("title {title:?}")),
                None => Err("no title was set".into()),
            },
            Assertion::Fg { row, col, color } => {
                let cell = self.cell(*row, *col)?;
                let got = cell.attributes().foreground();
                match got == *color {
                    true => Ok(()),
                    false => Err(format!("fg {}", Shown(got))),
                }
            }
        }
    }

    fn cell(&self, row: u16, col: u16) -> Result<fux_vt::CellRef<'_>, String> {
        let window = self.parser.screen().window();
        let (rows, cols) = (window.rows(), window.cols());
        window.cell(row, col).ok_or(match row < rows {
            true => format!("column {col} is off the {cols}-column screen"),
            false => format!("row {row} is off the {rows}-row screen"),
        })
    }

    /// The text of `count` glyphs from `col` on, a wide glyph's second half
    /// skipped: fewer if the row ends first.
    fn cells(&self, row: u16, col: u16, count: usize) -> Result<Vec<&str>, String> {
        let first = self.cell(row, col)?;
        if first.is_wide_continuation() {
            return Err(format!("column {col} is the second half of a wide glyph"));
        }
        let window = self.parser.screen().window();
        let glyphs = window.row(row).into_iter().flat_map(|line| line.glyphs());
        let glyphs = glyphs.skip_while(|(at, _)| *at < col);
        Ok(glyphs.take(count).map(|(_, c)| c.shown()).collect())
    }

    /// The screen as text, a line a row, numbered, with the cursor and title.
    pub fn dump(&self) -> String {
        let screen = self.parser.screen();
        let window = screen.window();
        let mut out = String::new();
        for (index, row) in window.iter() {
            out.push_str(&format!("{index:3}|{}\n", row.text().trim_end()));
        }
        let (row, col) = screen.cursor_position();
        out.push_str(&format!("cursor {row} {col}"));
        if let Some(title) = &self.title() {
            out.push_str(&format!(", title {title:?}"));
        }
        out.push('\n');
        out
    }
}
