//! What the unit tests feed a parser with: the calls a host makes,
//! [`Parser::process_until_stop`] until every byte is read; and a row's
//! text.
use crate::{Error, Options, Parser, Row, Sink, Size};

/// Takes nothing.
struct Deaf;
impl Sink for Deaf {}

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
    /// Reads every byte, as a host does, giving `sink` the replies and
    /// events: `Err(Error::Capacity)` if room was refused on the way.
    pub(crate) fn process_with(&mut self, bytes: &[u8], sink: &mut impl Sink) -> Result<(), Error> {
        let refused = self.refused();
        let mut rest = bytes;
        while let Some((taken, _)) = self.process_until_stop(rest, sink)? {
            rest = rest.get(taken..).unwrap_or_default();
        }
        match self.refused() == refused {
            true => Ok(()),
            false => Err(Error::Capacity),
        }
    }
    /// [`Parser::process_with`], the replies dropped.
    pub(crate) fn process(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.process_with(bytes, &mut Deaf)
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
