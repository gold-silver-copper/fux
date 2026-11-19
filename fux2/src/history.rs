//! Gathering a pane's scrollback as text, page by page. History rows keep
//! their numbers for good, so they may be taken from any page. Rows on the
//! screen do not, so they are taken only from the one answer that reaches the
//! end: that answer is a single observation, and every row comes out once.
use crate::wire::{Code, Failure, HistoryRow};

/// The most text one read gathers.
pub const MAX_TEXT: usize = 256 << 20;

/// A read in progress.
pub struct Gathered {
    text: String,
    next: u64,
}

impl Gathered {
    pub fn new(first: u64) -> Gathered {
        Gathered {
            text: String::new(),
            next: first,
        }
    }

    /// The row number to ask for next.
    pub fn next(&self) -> u64 {
        self.next
    }

    fn lost(&self, found: u64) -> Failure {
        let why = format!("row {} was asked for and row {found} came", self.next);
        Failure::new(Code::Stale, why)
    }

    /// Takes one answer; true once the read has reached the end.
    pub fn take(
        &mut self,
        end: u64,
        screen_end: u64,
        rows: &[HistoryRow],
    ) -> Result<bool, Failure> {
        let mut expected = self.next;
        for item in rows {
            if item.number != expected {
                return Err(self.lost(item.number));
            }
            expected = expected.saturating_add(1);
        }
        let complete = expected >= screen_end;
        let start = self.next;
        for item in rows.iter().filter(|item| complete || item.number < end) {
            if item.wrapped_at > 0 {
                self.text.push_str(&item.row.text_to(item.wrapped_at));
            } else {
                self.text.push_str(&item.row.text());
                self.text.push('\n');
            }
            self.next = item.number.saturating_add(1);
            if self.text.len() > MAX_TEXT {
                let why = format!("more than {MAX_TEXT} bytes of text");
                return Err(Failure::new(Code::Limit, why));
            }
        }
        if !complete && self.next == start {
            let why = format!("no progress past row {start}");
            return Err(Failure::new(Code::Stale, why));
        }
        Ok(complete)
    }

    /// The text, its last blank lines left out.
    pub fn finish(self) -> String {
        let mut text = self.text.trim_end_matches(['\n', ' ']).to_owned();
        if !text.is_empty() {
            text.push('\n');
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Model, checked_size};
    use crate::wire::{Answer, MAX_HISTORY_ROWS, PaneId};

    fn model(cols: u16, rows: u16) -> Result<Model, Failure> {
        Model::new(PaneId::random(), checked_size(cols, rows)?, 1 << 16)
    }

    fn feed(model: &mut Model, bytes: &[u8]) {
        let mut rest = bytes;
        while !rest.is_empty() {
            let (now, _) = rest.split_at_checked(64).unwrap_or((rest, &[]));
            let (taken, _) = model.parse(now, &mut ());
            rest = rest.get(taken..).unwrap_or_default();
        }
        model.advance();
    }

    type Page = (u64, u64, u64, Vec<HistoryRow>);

    fn page(model: &Model, epoch: u64, first: u64, count: u64) -> Result<Page, Failure> {
        let body = model.history(epoch, first, count)?;
        let Answer::History {
            start,
            end,
            screen_end,
            rows,
            ..
        } = Answer::decode(&body)?.accepted()?
        else {
            return Err(Failure::new(Code::Protocol, "not a history answer"));
        };
        Ok((start, end, screen_end, rows))
    }

    fn all_text(model: &Model) -> Result<String, Failure> {
        let (start, ..) = page(model, 0, 0, 0)?;
        let mut read = Gathered::new(start);
        loop {
            let (_, end, screen_end, rows) = page(model, 0, read.next(), MAX_HISTORY_ROWS)?;
            if read.take(end, screen_end, &rows)? {
                return Ok(read.finish());
            }
        }
    }

    /// B3: rows that move between pages still come out once each, in order.
    #[test]
    fn a_scroll_between_pages_repeats_nothing() -> Result<(), Failure> {
        let mut model = model(10, 3)?;
        feed(&mut model, b"H1\r\nH2\r\nA\r\nB\r\nC");
        let (start, end, screen_end, rows) = page(&model, 0, 0, 3)?;
        assert_eq!(start, 0);
        let mut read = Gathered::new(start);
        assert!(!read.take(end, screen_end, &rows)?);
        // The top two rows of the screen scroll away; the third stays put
        // and its number changes under the read.
        feed(&mut model, b"\x1b[1;2r\x1b[2;1H\n\n\x1b[r");
        loop {
            let (_, end, screen_end, rows) = page(&model, 0, read.next(), MAX_HISTORY_ROWS)?;
            if read.take(end, screen_end, &rows)? {
                break;
            }
        }
        let text = read.finish();
        let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
        assert_eq!(lines, ["H1", "H2", "A", "B", "C"], "{text:?}");
        Ok(())
    }

    /// B3: a hard reset is a new numbering, and a read in progress is told.
    #[test]
    fn a_reset_ends_a_read_in_progress() -> Result<(), Failure> {
        let mut model = model(10, 2)?;
        feed(&mut model, b"1\r\n2\r\n3\r\n4");
        feed(&mut model, b"\x1bc");
        let refused = model.history(0, 0, 1).err().map(|f| f.code);
        assert_eq!(refused, Some(Code::Stale));
        Ok(())
    }

    /// B4: a wide glyph that wrapped leaves no blank in the line.
    #[test]
    fn a_wrapped_wide_glyph_adds_no_space() -> Result<(), Failure> {
        let mut model = model(3, 2)?;
        feed(&mut model, "ab\u{754c}".as_bytes());
        assert_eq!(all_text(&model)?, "ab\u{754c}\n");
        Ok(())
    }

    /// B2: a page that gets nowhere ends the read.
    #[test]
    fn a_page_without_progress_is_refused() {
        let mut read = Gathered::new(5);
        assert!(read.take(5, 9, &[]).is_err());
    }
}
