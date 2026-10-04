//! fux-vt's behaviour oracle: the working tree's fux-vt beside fux-vt at a
//! pinned commit (`base-vt` in `Cargo.toml`, the merge base with `main` by
//! default), fed the same bytes, resizes and API calls, with everything
//! fux-vt's public API shows compared after every step. Any difference
//! fails, and `shrink` cuts the case down to the smallest that still
//! shows one.
//!
//! - `model`: what is compared, in one form for both sides.
//! - `side`: each side read into it, through its own public API.
//! - `case`: a case (a parser's size, history and options, and steps), how
//!   both are run through it, and how it is written down and read back.
//! - `observe`: what is read after each step, and how two readings are
//!   told apart.
//! - `exempt`: the approved exemptions, sequences of features added on
//!   purpose since the commit, taken out of what both sides are given.
//! - `cells`: fux-vt's standalone types (`Cells`, `Cell`), its constants
//!   and `continues_cluster`, compared on their own.
//! - `inputs`: the cases: the corpus, the random families shared with
//!   fux-vt-compare, a resize-heavy stream with history full, and the
//!   limits.
//! - `shrink`: the smallest case that still differs.
pub mod case;
pub mod cells;
pub mod exempt;
pub mod inputs;
pub mod model;
pub mod observe;
pub mod shrink;
mod side;

/// fux-vt-compare's random families, shared as they are written there.
#[path = "../../../fux-vt/compare/src/families.rs"]
pub mod families;
/// The characters whose clustering is interesting, as fux-vt's own tests
/// and fuzz targets draw them.
#[path = "../../../fux-vt/tests/corpus/graphemes.rs"]
pub mod graphemes;
/// fux-vt-compare's seeded source of choices, which the families draw from.
#[path = "../../../fux-vt/compare/src/rng.rs"]
mod rng;

pub use case::{Case, Count, Difference, Step, check};
pub use side::{base, work};

use model::{Error, Heard, Lookup, Marked, Row, Seen, Selection, Setup, State};

/// One side: a parser of one fux-vt, read into the model.
pub trait Side: Clone {
    /// `work` or `base`.
    const NAME: &'static str;
    /// A parser of `rows` by `cols` with `history` rows of history.
    fn new(rows: u16, cols: u16, history: usize, setup: &Setup) -> Result<Self, Error>;
    /// `Parser::process_with`, with what it gave the host.
    fn process(&mut self, bytes: &[u8], heard: &mut Vec<Heard>) -> Result<(), Error>;
    /// `Parser::process_until_frame`.
    fn process_until_frame(
        &mut self,
        bytes: &[u8],
        heard: &mut Vec<Heard>,
    ) -> Result<Option<usize>, Error>;
    /// `Parser::resize`.
    fn resize(&mut self, rows: u16, cols: u16) -> Result<(), Error>;
    /// Everything about the screen but its rows.
    fn state(&self) -> State;
    /// How many rows the screen retains: its history and its live rows.
    fn retained(&self) -> usize;
    /// Row `offset` from the bottom (`Screen::row_from_bottom`), read into
    /// `out`; `false` if there is none.
    fn row(&self, offset: usize, out: &mut Row) -> bool;
    /// Takes a mark of the screen as it is now, into `slot`.
    fn take_mark(&mut self, slot: usize);
    /// What each mark taken says now.
    fn marks(&self) -> Vec<Marked>;
    /// The rows at `offsets` from the bottom looked up by identity, and the
    /// oldest row of the last lookup, which may be gone.
    fn lookups(&mut self, offsets: &[usize]) -> Vec<Lookup>;
    /// A window and a copy from it; each cell of it too, if `cells`.
    fn copy(&self, selection: &Selection, cells: bool) -> Seen;
}
