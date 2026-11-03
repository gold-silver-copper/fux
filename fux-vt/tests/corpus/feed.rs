//! What the tests feed a parser with and read its screen by, over the
//! calls a host makes: [`Parser::process_all`], and the screen's window.
use fux_vt::{CellRef, Error, Hyperlink, Options, Parser, Row, Screen, Sink, Size};

/// Gives each reply to a closure.
struct Replies<F>(F);
impl<F: FnMut(&[u8])> Sink for Replies<F> {
    fn reply(&mut self, bytes: &[u8]) {
        (self.0)(bytes);
    }
}

/// A parser as the tests make and feed it.
pub trait Feed: Sized {
    /// A parser with [`Options::default`].
    fn new(size: Size, history_bytes: usize) -> Result<Self, Error>;
    /// [`Parser::process_all`], each reply given to `reply`.
    fn process_with_replies(&mut self, bytes: &[u8], reply: impl FnMut(&[u8]))
    -> Result<(), Error>;
    /// [`Parser::process_all`], the replies dropped.
    fn process(&mut self, bytes: &[u8]) -> Result<(), Error>;
}

impl Feed for Parser {
    fn new(size: Size, history_bytes: usize) -> Result<Self, Error> {
        Parser::with_options(size, history_bytes, Options::default())
    }
    fn process_with_replies(
        &mut self,
        bytes: &[u8],
        reply: impl FnMut(&[u8]),
    ) -> Result<(), Error> {
        self.process_all(bytes, &mut Replies(reply))
    }
    fn process(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.process_all(bytes, &mut ())
    }
}

/// A screen's cells as the tests read them.
pub trait Look {
    /// The cell at `row`, `col` of the screen.
    fn cell(&self, row: u16, col: u16) -> Option<CellRef<'_>>;
    /// The hyperlink of the cell at `row`, `col` of the screen.
    fn link(&self, row: u16, col: u16) -> Option<&Hyperlink>;
    /// Whether the line of the screen's row `row` goes on in the next
    /// (`Row::continues`).
    fn row_continues(&self, row: u16) -> bool;
}

impl Look for Screen {
    fn cell(&self, row: u16, col: u16) -> Option<CellRef<'_>> {
        self.window().row(row)?.cell(usize::from(col))
    }
    fn link(&self, row: u16, col: u16) -> Option<&Hyperlink> {
        self.window().row(row)?.link(usize::from(col))
    }
    fn row_continues(&self, row: u16) -> bool {
        self.window().row(row).is_some_and(|r| r.continues())
    }
}

/// A row's text: blanks and placeholders as spaces, trailing blanks kept.
pub fn cells_text(row: &Row<'_>) -> String {
    (row.cells().filter(|c| !c.is_wide_continuation()))
        .map(|c| c.shown())
        .collect()
}
