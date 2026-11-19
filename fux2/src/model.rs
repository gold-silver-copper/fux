//! A pane's terminal and what it has told its readers: the parser, the
//! version counter, and the row identities behind every token it gave out.
//! It does no I/O, so what it promises can be tested on its own.
use crate::wire::{
    Answer, Change, Code, Failure, HISTORY_PAGE, MAX_ANSWER, MAX_COLS, MAX_HISTORY_ROWS, MAX_ROWS,
    Meta, PaneId, Part, Row, State, Token, Writer, history_head, screen_head,
};
use fux_vt::keys::encode;
use fux_vt::{Feature, Identity, Mark, Mode, Options, Parser, RowId, Sink, Size, Stop};
use std::collections::VecDeque;

/// How many tokens a pane remembers well enough to answer with a patch.
const REMEMBERED: usize = 64;
/// How much output goes to the parser at a time: every byte of it could
/// ask for a reply of up to [`REPLY_MOST`] bytes.
pub const SLICE: usize = 512;
/// The longest reply one sequence gets (fux-vt's replies fit 64 bytes).
pub const REPLY_MOST: usize = 64;
/// Room needed for replies before the next [`SLICE`] is parsed.
pub const REPLY_NEED: usize = SLICE * REPLY_MOST;

/// One token given out: enough to say later what changed since.
struct Issued {
    version: u64,
    mark: Mark,
    size: Size,
    ids: Vec<RowId>,
}

/// A terminal, numbered in versions.
pub struct Model {
    parser: Parser,
    pub id: PaneId,
    version: u64,
    epoch: u64,
    issued: VecDeque<Issued>,
    state: State,
    limitations: Vec<String>,
}

/// Whether `cols` by `rows` is a size a pane may have.
pub fn checked_size(cols: u16, rows: u16) -> Result<Size, Failure> {
    let bad = || {
        Failure::new(
            Code::Invalid,
            format!("size {cols}x{rows}: 1 to {MAX_COLS} columns and 1 to {MAX_ROWS} rows"),
        )
    };
    if cols > MAX_COLS || rows > MAX_ROWS {
        return Err(bad());
    }
    Size::new(rows, cols).map_err(|_| bad())
}

impl Model {
    pub fn new(id: PaneId, size: Size, history: usize) -> Result<Model, Failure> {
        let identity = Identity {
            name: "fux2",
            version: env!("CARGO_PKG_VERSION"),
        };
        // Colours a program sets stay out (no `Feature::Palette`): a change
        // to the palette would recolour rows without changing them, which
        // no patch would carry.
        let options = Options::new()
            .with(Feature::ExtendedReplies)
            .with(Feature::ModeReports)
            .with(Feature::InBandResize)
            .with(Feature::SizeReports)
            .with(Feature::KittyKeyboard)
            .with(Feature::Reflow)
            .with(Feature::SettingReports)
            .with_identity(Some(identity));
        let parser = Parser::with_options(size, history, options)
            .map_err(|e| Failure::new(Code::Limit, format!("a terminal: {e}")))?;
        Ok(Model {
            parser,
            id,
            version: 1,
            epoch: 0,
            issued: VecDeque::new(),
            state: State::Running,
            limitations: Vec::new(),
        })
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn running(&self) -> bool {
        self.state == State::Running
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn size(&self) -> (u16, u16) {
        let size = self.parser.screen().size();
        (size.cols(), size.rows())
    }

    /// Notes something this terminal lost or cannot do, once.
    pub fn limitation(&mut self, text: String) {
        if !self.limitations.contains(&text) && self.limitations.len() < 16 {
            self.limitations.push(text);
            self.advance();
        }
    }

    /// Something a reader should see happened. A counter at its end freezes
    /// the pane rather than repeat a version.
    pub fn advance(&mut self) {
        match self.version.checked_add(1) {
            Some(next) => self.version = next,
            None => {
                self.state = State::Failed {
                    why: "limit: the version counter ran out".to_owned(),
                };
            }
        }
    }

    pub fn set_state(&mut self, state: State) {
        self.state = state;
        self.advance();
    }

    /// Parses `bytes`, at most a [`SLICE`], replies going to `sink`: how
    /// many it took, and whether its work budget stopped it.
    pub fn parse(&mut self, bytes: &[u8], sink: &mut impl Sink) -> (usize, bool) {
        match self.parser.process_until_stop(bytes, sink) {
            None => (bytes.len(), false),
            Some((taken, Stop::Budget)) => (taken, true),
            Some((taken, Stop::Begun | Stop::Ended)) => (taken, false),
        }
    }

    /// Whether the parser owes work with no new byte.
    pub fn owes_work(&self) -> bool {
        self.parser.has_work_left()
    }

    /// The token of now, remembered so that a later read can patch from it.
    pub fn token(&mut self) -> Token {
        if self.issued.back().is_none_or(|i| i.version != self.version) {
            if self.issued.len() >= REMEMBERED {
                self.issued.pop_front();
            }
            let screen = self.parser.screen();
            self.issued.push_back(Issued {
                version: self.version,
                mark: screen.mark(),
                size: screen.size(),
                ids: screen.window().iter().map(|(_, row)| row.id()).collect(),
            });
        }
        Token {
            pane: self.id,
            version: self.version,
        }
    }

    /// Refuses a token that is not of this pane or of the future.
    pub fn check_token(&self, token: Token) -> Result<(), Failure> {
        if token.pane != self.id {
            let why = format!("token {token} is of another pane; this is {}", self.id);
            return Err(Failure::new(Code::Replaced, why));
        }
        if token.version > self.version {
            let why = format!("token {token} is ahead of version {}", self.version);
            return Err(Failure::new(Code::Invalid, why));
        }
        Ok(())
    }

    /// Refuses a mutation whose expected token is out of date.
    pub fn check_expected(&self, expected: Option<Token>) -> Result<(), Failure> {
        let Some(token) = expected else {
            return Ok(());
        };
        self.check_token(token)?;
        match token.version == self.version {
            true => Ok(()),
            false => {
                let why = format!("token {token} is behind version {}", self.version);
                Err(Failure::new(Code::Stale, why))
            }
        }
    }

    fn meta(&self) -> Meta {
        let screen = self.parser.screen();
        let (cols, rows) = self.size();
        let (cursor_row, cursor_col) = screen.cursor_position();
        let history = screen.history_numbers();
        Meta {
            cols,
            rows,
            cursor_row,
            cursor_col,
            cursor_visible: screen.mode(Mode::ShowCursor),
            cursor_shape: screen.cursor_shape().number(),
            alternate: screen.mode(Mode::AlternateScreen),
            history_start: history.start,
            history_end: history.end,
            epoch: self.epoch,
            limitations: self.limitations.clone(),
            state: self.state.clone(),
        }
    }

    /// The answer to a screen read: one observation, encoded as it is.
    pub fn read(&mut self, since: Option<Token>) -> Result<Vec<u8>, Failure> {
        if let Some(token) = since {
            self.check_token(token)?;
        }
        let token = self.token();
        let meta = self.meta();
        let mut w = Writer::default();
        screen_head(&mut w, token, &meta);
        let screen = self.parser.screen();
        let window = screen.window();
        let held = since.and_then(|since| {
            let issued = self.issued.iter().find(|i| i.version == since.version);
            issued.filter(|i| i.size == screen.size())
        });
        if since.is_some_and(|since| since.version == self.version) {
            w.array(1);
            w.uint(0);
        } else if let Some(held) = held {
            let moved = screen.scrolled_since(held.mark);
            let moved = u16::try_from(moved)
                .ok()
                .filter(|m| *m < meta.rows)
                .unwrap_or(0);
            let changed: Vec<_> = window
                .iter()
                .filter(|(y, row)| {
                    let was = usize::from(*y).saturating_add(moved.into());
                    held.ids.get(was) != Some(&row.id()) || row.changed_since(held.mark)
                })
                .collect();
            w.array(3);
            w.uint(2);
            w.uint(moved.into());
            w.array(changed.len());
            for (y, row) in changed {
                w.array(2);
                w.uint(y.into());
                w.screen_row(&row);
            }
        } else {
            w.array(2);
            w.uint(1);
            w.array(meta.rows.into());
            for (_, row) in window.iter() {
                w.screen_row(&row);
            }
        }
        checked(w)
    }

    /// The answer to a history read.
    pub fn history(&self, epoch: u64, first: u64, count: u64) -> Result<Vec<u8>, Failure> {
        if epoch != self.epoch {
            let why = format!("epoch {epoch}: a resize made it {}", self.epoch);
            return Err(Failure::new(Code::Stale, why));
        }
        if count > MAX_HISTORY_ROWS {
            let why = format!("{count} rows, at most {MAX_HISTORY_ROWS}");
            return Err(Failure::new(Code::Invalid, why));
        }
        let screen = self.parser.screen();
        let range = screen.history_numbers();
        if first < range.start {
            let why = format!("row {first} is gone; {}..{} remain", range.start, range.end);
            return Err(Failure::new(Code::Gone, why));
        }
        // Numbers go on past history down the primary screen, and a row
        // keeps its number as it scrolls up into history.
        let kept = u64::try_from(screen.main_rows().len()).unwrap_or(u64::MAX);
        let screen_end = range.start.saturating_add(kept);
        let end = first.saturating_add(count).min(screen_end);
        let skip = usize::try_from(first.saturating_sub(range.start)).unwrap_or(usize::MAX);
        let mut rows = Writer::default();
        let mut count = 0usize;
        for (number, row) in (first..end.max(first)).zip(screen.main_rows().skip(skip)) {
            let wrapped_at = match row.continues() {
                true => u64::try_from(row.len()).unwrap_or(0),
                false => 0,
            };
            rows.array(3);
            rows.uint(number);
            rows.uint(wrapped_at);
            rows.screen_row(&row);
            count = count.saturating_add(1);
            if rows.0.len() >= HISTORY_PAGE {
                break;
            }
        }
        let mut w = Writer::default();
        let range = (range.start, range.end, screen_end);
        history_head(&mut w, self.epoch, range, count);
        w.0.extend_from_slice(&rows.0);
        checked(w)
    }

    /// `parts` as the bytes this program's modes make of them.
    pub fn encode(&self, parts: &[Part]) -> Result<Vec<u8>, Failure> {
        let screen = self.parser.screen();
        let mut out = Vec::new();
        for part in parts {
            match part {
                Part::Bytes(bytes) => out.extend_from_slice(bytes),
                Part::Key(name) => {
                    let press: fux_vt::keys::KeyPress = name
                        .parse()
                        .map_err(|e| Failure::new(Code::Invalid, format!("{e}")))?;
                    encode::key_bytes(press.into(), screen.key_mode(), &mut out);
                }
                Part::Paste(text) => {
                    encode::paste(text, screen.mode(Mode::BracketedPaste), &mut out);
                }
            }
        }
        Ok(out)
    }

    /// The terminal as it will be at `size`, nothing changed yet: a resize
    /// that the system then refuses leaves this one as it was.
    pub fn prepare_resize(&self, size: Size) -> Result<Parser, Failure> {
        let mut next = self.parser.clone();
        next.resize(size)
            .map_err(|e| Failure::new(Code::Limit, format!("resizing: {e}")))?;
        Ok(next)
    }

    /// Puts a prepared terminal in place: its new size is now the screen,
    /// at a new version. What it owes the program, if anything, is returned.
    pub fn commit_resize(&mut self, next: Parser) -> Option<Vec<u8>> {
        self.parser = next;
        self.epoch = self.epoch.saturating_add(1);
        self.advance();
        self.parser.resize_report()
    }
}

fn checked(w: Writer) -> Result<Vec<u8>, Failure> {
    match w.0.len() <= MAX_ANSWER {
        true => Ok(w.0),
        false => {
            let why = format!("an answer of {} bytes, at most {MAX_ANSWER}", w.0.len());
            Err(Failure::new(Code::Limit, why))
        }
    }
}

/// Brings `held`, a reader's rows, up to a change: what a viewer does, and
/// what the patch property is stated about. `rows` is the new screen's height.
pub fn apply(held: &mut Vec<Row>, rows: u16, change: Change) {
    match change {
        Change::Unchanged => {}
        Change::Whole(new) => *held = new,
        Change::Patch { moved, rows: new } => {
            let kept = std::mem::take(held);
            *held = kept.into_iter().skip(moved.into()).collect();
            held.resize(rows.into(), Row::default());
            for (y, row) in new {
                if let Some(slot) = held.get_mut(usize::from(y)) {
                    *slot = row;
                }
            }
        }
    }
    held.resize(rows.into(), Row::default());
}

/// Reads a screen answer back.
pub fn screen_of(body: &[u8]) -> Result<(Token, Meta, Change), Failure> {
    let answer = Answer::decode(body)?.accepted()?;
    let Answer::Screen {
        token,
        meta,
        change,
    } = answer
    else {
        return Err(Failure::new(Code::Protocol, "not a screen"));
    };
    Ok((token, *meta, change))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(cols: u16, rows: u16) -> Result<Model, Failure> {
        Model::new(PaneId::random(), checked_size(cols, rows)?, 1 << 16)
    }

    fn feed(model: &mut Model, bytes: &[u8]) {
        let mut rest = bytes;
        loop {
            let (now, _) = rest.split_at_checked(SLICE).unwrap_or((rest, &[]));
            let (taken, _) = model.parse(now, &mut ());
            rest = rest.get(taken..).unwrap_or_default();
            if rest.is_empty() && !model.owes_work() {
                break;
            }
        }
        model.advance();
    }

    /// A small generator of output that writes, scrolls, erases, switches
    /// screens and sets scroll regions, the moves a patch must survive.
    fn output(seed: &mut u64) -> Vec<u8> {
        let mut next = |bound: u64| {
            *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (*seed >> 33).checked_rem(bound).unwrap_or(0)
        };
        let mut out = Vec::new();
        for _ in 0..=next(6) {
            let piece = match next(9) {
                0 => format!("line {}\r\n", next(1000)),
                1 => "\r\n\r\n\r\n".to_owned(),
                2 => format!("\x1b[{};{}H\x1b[3{}mX", next(6), next(12), next(8)),
                3 => "\x1b[2J".to_owned(),
                4 => format!("\x1b[{};{}r", next(3), next(6)),
                5 => "\x1b[r".to_owned(),
                6 => if next(2) == 0 {
                    "\x1b[?1049h"
                } else {
                    "\x1b[?1049l"
                }
                .to_owned(),
                7 => format!("\x1b[{}S", next(4)),
                _ => format!("{}\u{4e16}", next(10)),
            };
            out.extend_from_slice(piece.as_bytes());
        }
        out
    }

    fn fresh_rows(model: &mut Model) -> Result<Vec<Row>, Failure> {
        let (_, meta, change) = screen_of(&model.read(None)?)?;
        let mut rows = Vec::new();
        apply(&mut rows, meta.rows, change);
        Ok(rows)
    }

    /// Whatever the program did, a reader holding the rows of any token it
    /// was given and applying the answer to `read(since)` has exactly what
    /// a fresh read shows.
    #[test]
    fn a_patch_rebuilds_the_screen_a_fresh_read_shows() -> Result<(), Failure> {
        let mut seed = 7;
        for _ in 0..40 {
            let mut model = model(20, 6)?;
            let mut readers: Vec<(Token, Vec<Row>)> = Vec::new();
            for step in 0..40 {
                feed(&mut model, &output(&mut seed));
                if step % 3 == 0 {
                    let (token, meta, change) = screen_of(&model.read(None)?)?;
                    let mut rows = Vec::new();
                    apply(&mut rows, meta.rows, change);
                    readers.push((token, rows));
                }
                let fresh = fresh_rows(&mut model)?;
                for (token, rows) in &mut readers {
                    let (next, meta, change) = screen_of(&model.read(Some(*token))?)?;
                    apply(rows, meta.rows, change);
                    *token = next;
                    assert_eq!(rows, &fresh, "after step {step}");
                }
            }
        }
        Ok(())
    }

    /// A plain scroll costs the new rows, not the screen.
    #[test]
    fn a_scroll_sends_only_what_is_new() -> Result<(), Failure> {
        let mut model = model(20, 6)?;
        feed(&mut model, b"a\r\nb\r\nc\r\nd\r\ne\r\nf");
        let (token, ..) = screen_of(&model.read(None)?)?;
        feed(&mut model, b"\r\ng");
        let (_, _, change) = screen_of(&model.read(Some(token))?)?;
        let Change::Patch { moved, rows } = change else {
            return Err(Failure::new(Code::Io, "a scroll answered with no patch"));
        };
        assert_eq!(moved, 1);
        assert!(rows.len() <= 2, "{rows:?}");
        Ok(())
    }

    /// A token from another life of the name is told apart from one that is
    /// merely old.
    #[test]
    fn tokens_of_another_life_are_refused() -> Result<(), Failure> {
        let (mut old, mut new) = (model(10, 3)?, model(10, 3)?);
        let token = old.token();
        let refused = new.read(Some(token)).err().map(|f| f.code);
        assert_eq!(refused, Some(Code::Replaced));
        new.advance();
        let ahead = Token {
            pane: new.id,
            version: 99,
        };
        assert_eq!(
            new.read(Some(ahead)).err().map(|f| f.code),
            Some(Code::Invalid)
        );
        let current = new.token();
        new.advance();
        let stale = new.check_expected(Some(current)).err().map(|f| f.code);
        assert_eq!(stale, Some(Code::Stale));
        Ok(())
    }

    /// The largest legal screen, every cell as costly as a screen can make
    /// it, still comes back whole.
    #[test]
    fn the_costliest_screen_fits_one_answer() -> Result<(), Failure> {
        let mut model = model(MAX_COLS, MAX_ROWS)?;
        let cluster: String = std::iter::once('e')
            .chain(std::iter::repeat_n('\u{301}', 62))
            .collect();
        let mut text = String::new();
        for n in 0..u32::from(MAX_COLS).saturating_mul(MAX_ROWS.into()) {
            let c = n.checked_rem(256).unwrap_or(0);
            text.push_str(&format!(
                "\x1b[1;2;3;4;5;7;8;9;38;2;{c};{c};{c};48;2;{c};{c};{c}m{cluster}"
            ));
        }
        feed(&mut model, text.as_bytes());
        let body = model.read(None)?;
        assert!(body.len() > 1 << 20, "{} bytes", body.len());
        screen_of(&body).map(drop)
    }
}
