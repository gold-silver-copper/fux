//! What the unit tests feed a parser with: the calls a host makes,
//! [`Parser::process_all`]; and a row's text.
use crate::{Error, Options, Parser, Row, Sink, Size};

/// Counts the pictures told not placed (`Sink::picture`).
#[derive(Default)]
pub(crate) struct Refused(pub(crate) u64);
impl Sink for Refused {
    fn picture(&mut self, placed: bool) {
        self.0 = self.0.saturating_add(u64::from(!placed));
    }
}

impl Parser {
    /// A parser with [`Options::default`].
    pub(crate) fn new(size: Size, history_bytes: usize) -> Result<Self, Error> {
        Self::with_options(size, history_bytes, Options::default())
    }
    /// [`Parser::process_all`], the replies dropped.
    pub(crate) fn process(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.process_all(bytes, &mut ())
    }
}

impl Row<'_> {
    /// The row's text: blanks and placeholders as spaces, trailing blanks
    /// kept.
    pub(crate) fn cells_text(&self) -> String {
        (self.cells().filter(|c| !c.is_wide_continuation()))
            .map(|c| c.shown())
            .collect()
    }
}
