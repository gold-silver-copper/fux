//! History's rows by identity (`THREATS.md` B40): its rows' identities
//! in spans of consecutive ones, as rows scrolled off a screen mostly
//! come, each span kept by its first identity. Identities are never
//! reused, so the spans' identities never overlap, whatever their order:
//! a row is found by the span its identity falls in and its place there,
//! one row read to make sure, with no assumption on how spans follow.

use std::collections::{BTreeMap, VecDeque};

/// What a span holds, at most: its entry in `by_id` (a key, a value and
/// its share of a B-tree node, counted at a node half full, and of the
/// nodes above), its place in `order` (with room for as many more), and
/// while history is made anew its place in `staged`.
pub(crate) const SPAN_BYTES: usize = 80;
/// The spans a history holds at no cost to its budget: 20 KiB of them,
/// more than a history that is not made to begin a span a row ever
/// holds, so that what it keeps does not depend on how its identities
/// run; past them, each costs `SPAN_BYTES` of the budget (`charged`).
pub(crate) const FREE_SPANS: usize = 256;

#[derive(Clone, Debug)]
pub(crate) struct Spans {
    /// The identity the newest span goes on with; `u64::MAX`, which no
    /// row has, while there is none.
    next: u64,
    /// Each span's first identity, and the number its first row came in
    /// history with.
    by_id: BTreeMap<u64, u64>,
    /// The spans' first rows' numbers and identities, oldest first, to
    /// let go those whose rows all went.
    order: VecDeque<(u64, u64)>,
    /// While history is made anew (`stage`), the spans begun, put in
    /// `by_id` at once when it is made (`finish`): a resize's rows come
    /// in many spans, as a line laid out again takes new identities for
    /// the rows it gains, and a sorted list builds a B-tree whole.
    staged: Option<Vec<(u64, u64)>>,
}

impl Default for Spans {
    fn default() -> Self {
        Self {
            next: u64::MAX,
            by_id: BTreeMap::new(),
            order: VecDeque::new(),
            staged: None,
        }
    }
}

impl Spans {
    /// Whether a row of identity `id` coming next begins a span; and the
    /// identity the next row goes on with.
    #[inline(always)]
    pub(crate) fn went_on(&mut self, id: u64) -> bool {
        let begins = id != self.next;
        self.next = id.wrapping_add(1);
        begins
    }
    /// The row of identity `id`, numbered `number`, begins a span; the
    /// spans of rows all let go before `first` go first.
    pub(crate) fn begin(&mut self, id: u64, number: u64, first: u64) {
        while self.order.get(1).is_some_and(|&(start, _)| start <= first) {
            if let Some((_, gone)) = self.order.pop_front() {
                self.by_id.remove(&gone);
            }
        }
        match &mut self.staged {
            Some(staged) => staged.push((id, number)),
            None => {
                self.by_id.insert(id, number);
            }
        }
        self.order.push_back((number, id));
    }
    /// History is made anew: the spans to come are kept apart until it is
    /// (`finish`), and found then.
    pub(crate) fn stage(&mut self) {
        self.staged = Some(Vec::new());
    }
    /// History is made: the spans kept apart go in `by_id`, built whole
    /// from them sorted if it holds none yet.
    pub(crate) fn finish(&mut self) {
        let Some(mut staged) = self.staged.take() else {
            return;
        };
        staged.sort_unstable();
        if self.by_id.is_empty() {
            self.by_id = staged.into_iter().collect();
        } else {
            self.by_id.extend(staged);
        }
    }
    /// How many spans it holds.
    pub(crate) fn len(&self) -> usize {
        self.by_id
            .len()
            .saturating_add(self.staged.as_ref().map_or(0, Vec::len))
    }
    /// What it costs history's budget: its spans past `FREE_SPANS`.
    pub(crate) fn charged(&self) -> usize {
        self.len()
            .saturating_sub(FREE_SPANS)
            .saturating_mul(SPAN_BYTES)
    }
    /// The number the row of identity `id` came with, if a span holds
    /// it; a row let go, or past its span's end, is not that row, which
    /// the caller reads to make sure.
    pub(crate) fn number_of(&self, id: u64) -> Option<u64> {
        let (&start, &number) = self.by_id.range(..=id).next_back()?;
        number.checked_add(id.checked_sub(start)?)
    }
}
