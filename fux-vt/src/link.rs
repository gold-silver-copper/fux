//! Hyperlinks (OSC 8, `references/modern/osc8_hyperlinks.md`): the links
//! of a grid's cells, held in runs per row beside the cells for the rows
//! that have any, each run with its link.
//!
//! A [`Hyperlink`] is the link itself, not a number to look up: its URI and
//! `id` are shared with the link the program opened, so a run costs no
//! allocation of its own, and a cell cannot name a link that is not there.
//! Only a cell with contents has a link: an erased cell keeps whatever link
//! it had, unread, until a glyph is printed there, which always writes its
//! link; so erasing part of a row, which blanks cells, never touches its
//! links. A whole row erased drops them, as nothing in them could be read
//! again (`Grid::erase`). The second half of a wide glyph has its first
//! half's link.
//!
//! What a grid's links cost is bounded: each new link is counted
//! (`Links::held`), and when one would pass the bound the rows' links are
//! counted again, and the oldest history rows lose theirs if need be
//! (`Grid::make_room`).

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

/// The longest URI kept, as VTE and iTerm2 bound it (the spec's "Length
/// limits"). A longer one opens no link.
pub const URI_LIMIT: usize = 2083;
/// The longest `id` kept, as VTE bounds it (the spec's "Length limits"). A
/// link with a longer one is not opened.
pub const ID_LIMIT: usize = 250;
/// The most heap a grid's links hold at once: each link's string, with
/// its URI and id and the `Arc`'s counts, and the map of those with an
/// `id`, its table as allocated, two tables while it grows (`Links`).
pub(crate) const LINK_BYTES: usize = 4 << 20;
/// What an `Arc<str>` allocates beyond its text: its two counts.
const ARC_HEADER: usize = 2 * size_of::<usize>();
/// After making room fails (every link left is on the screen), how many
/// more links are refused before trying again: the rows' links are counted
/// at most once in this many links.
const STALL: u32 = 256;

/// A cell's hyperlink: where it points, the program's `id` for it, if it
/// gave one, and a number that tells links apart. Cloning it is cheap: its
/// URI and `id` are one shared string, made with one allocation.
#[derive(Clone, PartialEq, Eq)]
pub struct Hyperlink {
    key: u64,
    /// The URI, after the `id` between two NULs if it has one: neither
    /// holds a NUL (`parse` keeps them printable ASCII), and a URI is never
    /// empty, so a link without an `id` costs no search to read its URI.
    text: Arc<str>,
}

impl Hyperlink {
    /// The link numbered `key` to `uri`, with `id` if it has one.
    pub(crate) fn new(key: u64, uri: &str, id: Option<&str>) -> Self {
        let text = match id {
            Some(id) => {
                let len = id.len().saturating_add(uri.len()).saturating_add(2);
                let mut text = String::with_capacity(len);
                for part in ["\0", id, "\0", uri] {
                    text.push_str(part);
                }
                Arc::from(text)
            }
            None => Arc::from(uri),
        };
        Self { key, text }
    }
    /// The `id` and the URI.
    fn parts(&self) -> (Option<&str>, &str) {
        match self.text.strip_prefix('\0') {
            Some(rest) => match rest.split_once('\0') {
                Some((id, uri)) => (Some(id), uri),
                None => (None, rest),
            },
            None => (None, &self.text),
        }
    }
    /// The URI, as the program sent it: at most [`URI_LIMIT`] bytes, each a
    /// printable ASCII character.
    pub fn uri(&self) -> &str {
        self.parts().1
    }
    /// The `id` parameter the program gave the link, if it gave a nonempty
    /// one: at most [`ID_LIMIT`] printable ASCII bytes.
    pub fn id(&self) -> Option<&str> {
        self.parts().0
    }
    /// A number that identifies the link among the parser's links, never
    /// given to another: cells with the same key are one link, as the spec
    /// groups them. Each `OSC 8` with a URI and no `id` opens a link of its
    /// own; one with an `id` and a URI the screen still holds a link for
    /// (in its history too; each screen holds its own), the same link
    /// again.
    pub fn key(&self) -> u64 {
        self.key
    }
    /// What the link's string allocates: its URI and `id`, as `text` holds
    /// them, and the `Arc`'s counts.
    fn cost(&self) -> usize {
        self.text.len().saturating_add(ARC_HEADER)
    }
}

/// As its fields: the key, the URI and the `id`.
impl std::fmt::Debug for Hyperlink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hyperlink")
            .field("key", &self.key())
            .field("uri", &self.uri())
            .field("id", &self.id())
            .finish()
    }
}

/// A link with an `id`, found by its `id` and URI (`Links::ids`).
#[derive(Clone, Debug)]
struct ById(Hyperlink);

impl PartialEq for ById {
    fn eq(&self, other: &Self) -> bool {
        self.0.text == other.0.text
    }
}

impl Eq for ById {}

impl std::hash::Hash for ById {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.text.hash(state);
    }
}

/// A row's links: runs of cells with one link each, left to right and
/// apart; a cell in no run has none. A row of the screen keeps its list's
/// memory for the slot's next row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RowLinks(Vec<Run>);

/// A history row's links, as `RowLinks` keeps them, in the memory they
/// need: one run, as a row with one link has, in place, with no memory of
/// its own, and more in a list of their own size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeptLinks {
    One(Run),
    Many(Box<[Run]>),
}

/// A row's links to read: its runs, left to right.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Runs<'a>(&'a [Run]);

/// Cells `start..end` of a row, and their link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    start: u16,
    end: u16,
    link: Hyperlink,
}

const _: () = assert!(size_of::<Run>() == 32 && size_of::<KeptLinks>() == 32);

impl KeptLinks {
    /// The links `links` has, taken out of it, which keeps its memory;
    /// `None` if it has none.
    pub(crate) fn take(links: &mut RowLinks) -> Option<Self> {
        match links.0.len() {
            0 => None,
            1 => links.0.pop().map(Self::One),
            n => {
                // Moved out, last first, into a list of their own size.
                let mut runs = Vec::with_capacity(n);
                runs.extend(std::iter::from_fn(|| links.0.pop()));
                runs.reverse();
                Some(Self::Many(runs.into_boxed_slice()))
            }
        }
    }
    /// A copy of `runs`; `None` if there are none.
    pub(crate) fn of(runs: Runs<'_>) -> Option<Self> {
        match runs.0 {
            [] => None,
            [run] => Some(Self::One(run.clone())),
            runs => Some(Self::Many(runs.into())),
        }
    }
    /// Its runs.
    pub(crate) fn runs(&self) -> Runs<'_> {
        match self {
            Self::One(run) => Runs(std::slice::from_ref(run)),
            Self::Many(runs) => Runs(runs),
        }
    }
    /// The bytes its runs take beyond itself.
    pub(crate) fn heap(&self) -> usize {
        match self {
            Self::One(_) => 0,
            Self::Many(runs) => runs.len().saturating_mul(size_of::<Run>()),
        }
    }
}

impl RowLinks {
    /// Its runs.
    #[inline]
    pub(crate) fn runs(&self) -> Runs<'_> {
        Runs(&self.0)
    }
    /// Whether no cell has a link.
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// Drops every link, keeping the memory for the slot's next row.
    pub(crate) fn clear(&mut self) {
        self.0.clear();
    }
    /// Gives the cells `span` link `link`, or none; whether a cell's link
    /// changed. A span past what a row holds changes nothing. Inlined for
    /// a row written left to right; the rest is out of line (`overwrite`).
    #[inline]
    pub(crate) fn set(&mut self, span: Range<usize>, link: Option<&Hyperlink>) -> bool {
        let (Ok(start), Ok(end)) = (u16::try_from(span.start), u16::try_from(span.end)) else {
            return false;
        };
        if start >= end {
            return false;
        }
        // Printed past every run.
        if self.0.last().is_none_or(|last| last.end <= start) {
            let Some(link) = link else {
                return false;
            };
            self.push(start, end, link.clone());
            return true;
        }
        self.overwrite(start..end, link)
    }
    /// `set` over cells some run reaches.
    #[inline(never)]
    fn overwrite(&mut self, span: Range<u16>, link: Option<&Hyperlink>) -> bool {
        if self.has(span.clone(), link) {
            return false;
        }
        let Range { start, end } = span;
        self.remake(|run| {
            let before = (run.start, run.end.min(start));
            [before, (run.start.max(end), run.end), (0, 0)]
        });
        if let Some(link) = link {
            let link = link.clone();
            self.0.push(Run { start, end, link });
        }
        self.tidy();
        true
    }
    /// Whether every cell of `span` has `link`.
    fn has(&self, span: Range<u16>, link: Option<&Hyperlink>) -> bool {
        let first = self.0.partition_point(|run| run.end <= span.start);
        let mut runs =
            (self.0.get(first..).unwrap_or_default().iter()).take_while(|run| run.start < span.end);
        match link {
            None => runs.next().is_none(),
            Some(link) => {
                let mut at = span.start;
                runs.all(|run| {
                    let joins = run.start <= at && run.link == *link;
                    at = run.end;
                    joins
                }) && at >= span.end
            }
        }
    }
    /// Puts a run of `link` after the runs, joined to the last if it is
    /// the same link and they touch.
    fn push(&mut self, start: u16, end: u16, link: Hyperlink) {
        match self.0.last_mut() {
            Some(last) if last.end == start && last.link == link => last.end = end,
            Some(_) | None => self.0.push(Run { start, end, link }),
        }
    }
    /// Remakes each run as the cells `parts` gives of it, each a run of its
    /// link if it has any, in place: a run is edited, and one that parts in
    /// more leaves the others after the runs, for `tidy`.
    fn remake(&mut self, parts: impl Fn(&Run) -> [(u16, u16); 3]) {
        for i in 0..self.0.len() {
            let Some(run) = self.0.get_mut(i) else {
                break;
            };
            let mut cells = parts(run)
                .into_iter()
                .filter(|(start, end)| start < end)
                .peekable();
            (run.start, run.end) = cells.next().unwrap_or((0, 0));
            if cells.peek().is_some() {
                let link = run.link.clone();
                for (start, end) in cells {
                    let link = link.clone();
                    self.0.push(Run { start, end, link });
                }
            }
        }
    }
    /// Puts the runs in order again, those left empty gone and those that
    /// touch with one link joined.
    fn tidy(&mut self) {
        self.0.retain(|run| run.start < run.end);
        self.0.sort_unstable_by_key(|run| run.start);
        self.0.dedup_by(|next, kept| {
            let joins = kept.end == next.start && kept.link == next.link;
            if joins {
                kept.end = next.end;
            }
            joins
        });
    }
    /// ICH and DCH: the cells from `col` to `end` turn `count` places right
    /// to insert, left to delete, with their links; what turns past `end`
    /// is gone, and the cells it leaves have none. Out of line, as only
    /// rows with links call it.
    #[inline(never)]
    pub(crate) fn shift(&mut self, col: usize, count: usize, insert: bool, end: usize) {
        let clamp = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
        let (col, end) = (clamp(col), clamp(end));
        let count = clamp(count).min(end.saturating_sub(col));
        // The part before `col`, the part from it to `end`, turned, and the
        // part past `end`.
        self.remake(|run| {
            let turned = if insert {
                let start = run.start.max(col).saturating_add(count);
                (start, run.end.min(end).saturating_add(count).min(end))
            } else {
                let start = run.start.max(col.saturating_add(count));
                let end = run.end.min(end).saturating_sub(count);
                (start.saturating_sub(count), end)
            };
            [
                (run.start, run.end.min(col)),
                turned,
                (run.start.max(end), run.end),
            ]
        });
        self.tidy();
    }
}

impl<'a> Runs<'a> {
    /// The link of the cell at `col`.
    pub(crate) fn get(self, col: usize) -> Option<&'a Hyperlink> {
        let col = u16::try_from(col).ok()?;
        let at = self.0.partition_point(|run| run.end <= col);
        self.0
            .get(at)
            .filter(|run| run.start <= col)
            .map(|run| &run.link)
    }
    /// Each link of the cells `cells`, with the cells that have it there,
    /// counted from the first of `cells`.
    pub(crate) fn within(
        self,
        cells: Range<usize>,
    ) -> impl Iterator<Item = (Range<usize>, &'a Hyperlink)> {
        self.0.iter().filter_map(move |run| {
            let start = usize::from(run.start).max(cells.start);
            let end = usize::from(run.end).min(cells.end);
            let at = start.saturating_sub(cells.start);
            (start < end).then(|| (at..at.saturating_add(end.saturating_sub(start)), &run.link))
        })
    }
    /// Every link of the row, once a run.
    pub(crate) fn links(self) -> impl Iterator<Item = &'a Hyperlink> {
        self.0.iter().map(|run| &run.link)
    }
    /// The links of the cells before `width`.
    pub(crate) fn clipped(self, width: usize) -> RowLinks {
        let width = u16::try_from(width).unwrap_or(u16::MAX);
        let mut links = RowLinks(self.0.to_vec());
        links.remake(|run| [(run.start, run.end.min(width)), (0, 0), (0, 0)]);
        links.tidy();
        links
    }
}

/// The buckets a `HashSet` of `capacity` has: a power of two, with room
/// for an eighth more than it holds (hashbrown's), at least 4 once it has
/// any.
fn buckets(capacity: usize) -> usize {
    match capacity {
        0 => 0,
        1..8 => capacity.saturating_add(1).next_power_of_two().max(4),
        _ => (capacity.saturating_mul(8) / 7).next_power_of_two(),
    }
}

/// What a table of `buckets` allocates for `Links::ids`: a link and a
/// control byte a bucket, and a group's control bytes more.
fn table_bytes(buckets: usize) -> usize {
    match buckets {
        0 => 0,
        _ => (buckets.saturating_mul(size_of::<ById>().saturating_add(1))).saturating_add(16),
    }
}

/// A grid's links: what they cost, those with an `id` by `id` and URI, and
/// the link the program has open as the grid has it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Links {
    /// What the links held cost: the strings of those the rows had when
    /// they were last counted (`sweep`) and of every link made since, and
    /// `ids`'s table (`table_bytes` of `buckets`).
    held: usize,
    /// The buckets `ids`'s table has, as allocated: counted when it is
    /// made, grown or made smaller, as removals leave it as it is while
    /// its `capacity` counts their tombstones out.
    buckets: usize,
    /// The links with an `id`, by `id` and URI, until the rows are counted
    /// without them.
    ids: HashSet<ById>,
    /// Links still to refuse before making room is tried again.
    stalled: u32,
    /// The link the program has open (`Screen::link`), as the grid has it.
    pub(crate) open: Held,
}

impl Links {
    /// Whether no row has a link: every link a row has is held, and
    /// costs something.
    #[inline]
    pub(crate) fn none(&self) -> bool {
        self.held == 0
    }
    /// The held link with this `id` and URI.
    pub(crate) fn find(&self, opened: &Hyperlink) -> Option<&Hyperlink> {
        let found = self.ids.get(&ById(opened.clone()));
        found.map(|link| &link.0)
    }
    /// A new link, the link opened shared; `None` if it does not fit
    /// without making room. One with an `id` that grows `ids` fits only if
    /// the old table and the new, both held while it grows, do.
    pub(crate) fn insert(&mut self, opened: &Hyperlink) -> Option<Hyperlink> {
        let link = opened.clone();
        let held = self.held.saturating_add(link.cost());
        // With no room left, the table is made again, as large or twice as
        // large, the old one held until the new one is filled.
        let grows = link.id().is_some() && self.ids.len() >= self.ids.capacity();
        let grown = match grows {
            true => table_bytes(self.buckets.saturating_mul(2).max(4)),
            false => 0,
        };
        if held.saturating_add(grown) > LINK_BYTES {
            return None;
        }
        if grows {
            self.ids.try_reserve(1).ok()?;
            // A table just made holds no tombstone: its capacity is all
            // its buckets'.
            let made = buckets(self.ids.capacity());
            self.held =
                (held.saturating_sub(table_bytes(self.buckets))).saturating_add(table_bytes(made));
            self.buckets = made;
        } else {
            self.held = held;
        }
        if link.id().is_some() {
            self.ids.replace(ById(link.clone()));
        }
        Some(link)
    }
    /// Counts again the links `rows` have, each row with its index in a
    /// history of `len` rows, oldest first, or `None` for a row of the
    /// screen, after them; and how many of the oldest history rows must
    /// lose their links for the links left to cost at most half the bound,
    /// as many as there are if no number does. What the links left cost is
    /// held, and the links with an `id` that no row is left with go.
    pub(crate) fn sweep<'a, I>(&mut self, rows: impl Fn() -> I, len: usize) -> usize
    where
        I: Iterator<Item = (Option<usize>, Runs<'a>)>,
    {
        // Each link once, with the newest row that has it, a row of the
        // screen newer than any: by key, which links take as they open,
        // so mostly in order already. The list is made as long as the
        // runs are at once, the rows read once more to count them, as it
        // would be held twice while it grew; and rows and costs are 32
        // bits (a history holds at most `grid::MAX_ROWS` rows, and a link
        // costs less than 4 KiB): it is small beside the links.
        let runs = rows().fold(0usize, |n, (_, links)| n.saturating_add(links.0.len()));
        let mut seen: Vec<(u64, u32, u32)> = Vec::with_capacity(runs);
        for (row, links) in rows() {
            let row = row.map_or(u32::MAX, |row| u32::try_from(row).unwrap_or(u32::MAX - 1));
            seen.extend(links.links().map(|link| {
                let cost = u32::try_from(link.cost()).unwrap_or(u32::MAX);
                (link.key(), row, cost)
            }));
        }
        // Each run, each history row's share, each `id` and the two sorts'
        // steps, ⌈log2 runs⌉ × runs ÷ 16 each (threats.md B16): by key, and
        // the rows' by row.
        let log = usize::try_from(seen.len().checked_ilog2().unwrap_or(0)).unwrap_or(0);
        let sort = log.saturating_add(1).saturating_mul(seen.len()) / 16;
        spend!(
            links,
            seen.len()
                .saturating_add(len)
                .saturating_add(self.ids.len())
                .saturating_add(sort.saturating_mul(2))
        );
        seen.sort_unstable();
        seen.dedup_by(|next, kept| {
            let same = next.0 == kept.0;
            if same {
                kept.1 = next.1;
            }
            same
        });
        // The history rows lose their links oldest first, each taking the
        // links no newer row has, until what is left costs half the bound,
        // its table counted as it is now: the links by their newest row,
        // which only rows with links have, so that nothing is kept a
        // history row.
        let half = (LINK_BYTES / 2).saturating_sub(table_bytes(self.buckets));
        let wide = |n: u32| usize::try_from(n).unwrap_or(usize::MAX);
        let len = u32::try_from(len).unwrap_or(u32::MAX - 1);
        let mut live = (seen.iter()).fold(0usize, |live, &(_, _, c)| live.saturating_add(wide(c)));
        let mut lose = 0u32;
        let mut kept = seen.as_mut_slice();
        if live > half {
            lose = len;
            kept.sort_unstable_by_key(|&(_, row, _)| row);
            for leaving in kept.chunk_by(|a, b| a.1 == b.1) {
                let Some(&(_, row, _)) = leaving.first().filter(|(_, row, _)| *row < len) else {
                    break;
                };
                let cost =
                    (leaving.iter()).fold(0usize, |sum, &(_, _, c)| sum.saturating_add(wide(c)));
                live = live.saturating_sub(cost);
                if live <= half {
                    lose = row.saturating_add(1);
                    break;
                }
            }
            // The links rows from `lose` on keep, by key again.
            let from = kept.partition_point(|&(_, row, _)| row < lose);
            kept = kept.get_mut(from..).unwrap_or_default();
            kept.sort_unstable();
        }
        self.ids.retain(|ById(link)| {
            let at = kept.binary_search_by_key(&link.key(), |&(key, _, _)| key);
            at.ok()
                .and_then(|at| kept.get(at))
                .is_some_and(|&(_, row, _)| row >= lose)
        });
        // The table gives back what it holds past twice what it keeps, if
        // the smaller one fits beside it while both are held.
        if self.ids.is_empty() {
            self.ids = HashSet::new();
            self.buckets = 0;
        } else if buckets(self.ids.len()).saturating_mul(2) < self.buckets {
            let smaller = table_bytes(buckets(self.ids.len()));
            if live
                .saturating_add(table_bytes(self.buckets))
                .saturating_add(smaller)
                <= LINK_BYTES
            {
                self.ids.shrink_to(self.ids.len());
                self.buckets = buckets(self.ids.capacity());
            }
        }
        self.held = live.saturating_add(table_bytes(self.buckets));
        wide(lose)
    }
    /// Whether making room may be tried now, counting this try: after one
    /// fails, only once in [`STALL`] links.
    pub(crate) fn may_make_room(&mut self) -> bool {
        match self.stalled.checked_sub(1) {
            Some(left) => {
                self.stalled = left;
                false
            }
            None => true,
        }
    }
    /// Making room failed: refuse the next links without trying.
    pub(crate) fn stall(&mut self) {
        self.stalled = STALL;
    }
    /// What the links held cost, and what `live` cost, each link once: the
    /// links the rows have, which are held.
    #[cfg(test)]
    pub(crate) fn costs<'a>(&self, live: impl Iterator<Item = &'a Hyperlink>) -> (usize, usize) {
        let live: std::collections::HashMap<u64, usize> =
            live.map(|link| (link.key(), link.cost())).collect();
        (self.held, live.values().sum())
    }
}

/// The links' part of the invariant checker (threats.md section 6).
#[cfg(feature = "checks")]
impl Runs<'_> {
    /// 24: the runs lie inside a row `width` wide, each of a cell or more,
    /// left to right and apart, with links within the spec's limits.
    pub(crate) fn check(self, width: usize) -> Result<(), String> {
        let mut end = 0u16;
        for run in self.0 {
            crate::check::ensure(
                end <= run.start && run.start < run.end && usize::from(run.end) <= width,
                || {
                    format!(
                        "run {}..{} after {end}, in {width} cells",
                        run.start, run.end
                    )
                },
            )?;
            end = run.end;
            let (uri, id) = (run.link.uri().len(), run.link.id().map_or(0, str::len));
            crate::check::ensure(uri <= URI_LIMIT && id <= ID_LIMIT, || {
                format!("a link of {uri} and {id} bytes")
            })?;
        }
        Ok(())
    }
}

#[cfg(feature = "checks")]
impl Links {
    /// 23 and 24: what the links held cost is at most the bound, and at
    /// least what the links `rows` have cost, each link once; each with an
    /// `id` is found by it.
    pub(crate) fn check<'a>(&self, rows: impl Iterator<Item = Runs<'a>>) -> Result<(), String> {
        let mut costs = std::collections::HashMap::new();
        for links in rows {
            for link in links.links() {
                costs.insert(link.key(), link.cost());
                if link.id().is_some() {
                    let found = self.find(link).map(Hyperlink::key);
                    crate::check::ensure(found == Some(link.key()), || {
                        format!("link {} with an id is not found by it", link.key())
                    })?;
                }
            }
        }
        let live = costs
            .values()
            .fold(0, |sum: usize, cost| sum.saturating_add(*cost));
        // Its capacity counts tombstones out: the table has at least the
        // buckets it says, and what is counted is what it has.
        let map = table_bytes(self.buckets);
        let least = buckets(self.ids.capacity());
        crate::check::ensure(
            least <= self.buckets
                && live.saturating_add(map) <= self.held
                && self.held <= LINK_BYTES,
            || {
                format!(
                    "{} bytes of links held, the rows' cost {live} and the table's {map}",
                    self.held
                )
            },
        )
    }
}

/// What the open link is in one grid.
#[derive(Clone, Debug, Default)]
pub(crate) enum Held {
    /// Not printed there yet.
    #[default]
    Pending,
    /// This link.
    At(Hyperlink),
    /// The grid had no room for it: cells printed there have no link.
    Refused,
}

/// What `OSC 8 ; params ; URI` asks for: the link to open, or `None` to
/// close it. A URI or `id` past its limit, or with a byte that is not
/// printable ASCII (which the spec leaves undefined, and which could not be
/// passed on to another terminal intact), opens nothing.
pub(crate) fn parse(payload: &[u8]) -> Option<(&str, Option<&str>)> {
    let (params, uri) = crate::split_semicolon(payload)?;
    // `id=value` among `key=value` pairs separated by `:`; an empty id is
    // no id.
    let id = params
        .split(|b| *b == b':')
        .find_map(|param| param.strip_prefix(b"id="))
        .filter(|id| !id.is_empty());
    let printable = |bytes: &[u8]| bytes.iter().all(|b| (0x20..=0x7e).contains(b));
    if uri.is_empty()
        || uri.len() > URI_LIMIT
        || !printable(uri)
        || id.is_some_and(|id| id.len() > ID_LIMIT || !printable(id))
    {
        return None;
    }
    let uri = str::from_utf8(uri).ok()?;
    let id = id.map(str::from_utf8).transpose().ok()?;
    Some((uri, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sweep that lets most links with an id go leaves the table as
    /// large as it was (a `HashSet` gives no room back on removals), so it
    /// is made smaller, and counted as made: `capacity` alone, which
    /// counts the removed entries' tombstones out (896 down to 412 here),
    /// took the table for half as large, kept it, and counted it so.
    #[test]
    fn the_id_table_is_counted_as_allocated_after_a_sweep() {
        let mut links = Links::default();
        let made: Vec<Hyperlink> = (0..896u64)
            .filter_map(|i| links.insert(&Hyperlink::new(i, "u", Some(&i.to_string()))))
            .collect();
        assert_eq!(made.len(), 896);
        let full = buckets(links.ids.capacity());
        assert_eq!(full, 1024, "the table after the 896th link");
        let mut row = RowLinks::default();
        for (i, link) in made.iter().take(200).enumerate() {
            assert!(row.set(i * 2..i * 2 + 1, Some(link)));
        }
        let lose = links.sweep(|| std::iter::once((None, row.runs())), 0);
        assert_eq!((lose, links.ids.len()), (0, 200));
        // Kept to 200, the table is made smaller, to 256 buckets: a table
        // just made, its capacity all its buckets' (7 in 8); and held
        // counts it.
        let now = buckets(links.ids.capacity());
        assert_eq!((now, links.ids.capacity()), (256, 224), "made smaller");
        let strings = made.iter().take(200).map(Hyperlink::cost).sum::<usize>();
        assert_eq!(links.held, strings + table_bytes(now));
    }

    #[test]
    fn a_payload_opens_its_link_or_closes() {
        assert_eq!(parse(b";http://a"), Some(("http://a", None)));
        assert_eq!(parse(b"id=x;http://a"), Some(("http://a", Some("x"))));
        assert_eq!(
            parse(b"foo=bar:id=x:baz=1;http://a;b"),
            Some(("http://a;b", Some("x")))
        );
        // An empty id is no id; an empty URI closes the link.
        assert_eq!(parse(b"id=;http://a"), Some(("http://a", None)));
        assert_eq!(parse(b";"), None);
        assert_eq!(parse(b"id=x;"), None);
        assert_eq!(parse(b""), None);
        // Past the limits, or not printable ASCII, nothing is opened.
        let long: Vec<u8> = std::iter::repeat_n(b'a', URI_LIMIT).collect();
        let mut payload = b";".to_vec();
        payload.extend_from_slice(&long);
        assert!(parse(&payload).is_some());
        payload.push(b'a');
        assert_eq!(parse(&payload), None);
        let mut payload = b"id=".to_vec();
        payload.extend(std::iter::repeat_n(b'i', ID_LIMIT));
        payload.extend_from_slice(b";u");
        assert!(parse(&payload).is_some());
        let mut payload = b"id=i".to_vec();
        payload.extend(std::iter::repeat_n(b'i', ID_LIMIT));
        payload.extend_from_slice(b";u");
        assert_eq!(parse(&payload), None);
        assert_eq!(parse(";http://é".as_bytes()), None);
        assert_eq!(parse(b";http://a\x07b"), None);
        assert_eq!(parse(b"id=\x01;http://a"), None);
    }
}

/// The links' checks, each shown links with their one fault (HAR-26).
#[cfg(all(test, feature = "checks"))]
mod faults {
    use super::*;

    fn link(key: u64, id: Option<&str>) -> Hyperlink {
        Hyperlink::new(key, "http://a", id)
    }

    #[test]
    fn runs_out_of_order_or_past_the_row_are_faults() {
        let mut row = RowLinks::default();
        assert!(row.set(0..2, Some(&link(1, None))));
        assert!(row.set(3..4, Some(&link(2, None))));
        assert_eq!(row.runs().check(4), Ok(()));
        assert!(row.runs().check(3).is_err(), "past the row");
        row.0.reverse();
        assert!(row.runs().check(4).is_err(), "out of order");
    }

    #[test]
    fn links_held_for_less_than_the_rows_cost_or_lost_ids_are_faults() {
        let mut links = Links::default();
        let opened = link(1, Some("i"));
        let Some(held) = links.insert(&opened) else {
            return;
        };
        let mut row = RowLinks::default();
        assert!(row.set(0..1, Some(&held)));
        assert_eq!(links.check([row.runs()].into_iter()), Ok(()));
        links.held = 0;
        assert!(
            links.check([row.runs()].into_iter()).is_err(),
            "held too little"
        );
        links.held = held.cost();
        links.ids.clear();
        assert!(
            links.check([row.runs()].into_iter()).is_err(),
            "an id not found"
        );
    }
}
