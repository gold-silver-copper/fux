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

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

/// The longest URI kept, as VTE and iTerm2 bound it (the spec's "Length
/// limits"). A longer one opens no link.
pub const URI_LIMIT: usize = 2083;
/// The longest `id` kept, as VTE bounds it (the spec's "Length limits"). A
/// link with a longer one is not opened.
pub const ID_LIMIT: usize = 250;
/// The most bytes of URIs and ids a grid's links hold at once, each link
/// counted with `LINK_COST` more for what holds it.
pub(crate) const LINK_BYTES: usize = 4 << 20;
/// What a link costs beyond its URI and id: what holds it, and its place
/// in the map of ids.
const LINK_COST: usize = 64;
/// After making room fails (every link left is on the screen), how many
/// more links are refused before trying again: the rows' links are counted
/// at most once in this many links.
const STALL: u32 = 256;

/// A cell's hyperlink: where it points, the program's `id` for it, if it
/// gave one, and a number that tells links apart. Cloning it is cheap: the
/// URI and `id` are shared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hyperlink {
    key: u64,
    uri: Arc<str>,
    id: Option<Arc<str>>,
}

impl Hyperlink {
    /// The URI, as the program sent it: at most [`URI_LIMIT`] bytes, each a
    /// printable ASCII character.
    pub fn uri(&self) -> &str {
        &self.uri
    }
    /// The `id` parameter the program gave the link, if it gave a nonempty
    /// one: at most [`ID_LIMIT`] printable ASCII bytes.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
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
    /// What the link costs its grid.
    fn cost(&self) -> usize {
        cost(&self.uri, self.id.as_deref())
    }
}

/// What a link of `uri` and `id` costs its grid.
fn cost(uri: &str, id: Option<&str>) -> usize {
    uri.len()
        .saturating_add(id.map_or(0, str::len))
        .saturating_add(LINK_COST)
}

/// A row's links: runs of cells with one link each, left to right and
/// apart; a cell in no run has none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RowLinks(Vec<Run>);

/// Cells `start..end` of a row, and their link.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Run {
    start: u16,
    end: u16,
    link: Hyperlink,
}

impl RowLinks {
    /// Whether no cell has a link.
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// Drops every link, keeping the memory for the slot's next row.
    pub(crate) fn clear(&mut self) {
        self.0.clear();
    }
    /// The link of the cell at `col`.
    pub(crate) fn get(&self, col: usize) -> Option<&Hyperlink> {
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
        &self,
        cells: Range<usize>,
    ) -> impl Iterator<Item = (Range<usize>, &Hyperlink)> {
        self.0.iter().filter_map(move |run| {
            let start = usize::from(run.start).max(cells.start);
            let end = usize::from(run.end).min(cells.end);
            let at = start.saturating_sub(cells.start);
            (start < end).then(|| (at..at.saturating_add(end.saturating_sub(start)), &run.link))
        })
    }
    /// Every link of the row, once a run.
    pub(crate) fn links(&self) -> impl Iterator<Item = &Hyperlink> {
        self.0.iter().map(|run| &run.link)
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
    /// The links of the cells before `width`.
    pub(crate) fn clipped(&self, width: usize) -> RowLinks {
        let width = u16::try_from(width).unwrap_or(u16::MAX);
        let mut links = self.clone();
        links.remake(|run| [(run.start, run.end.min(width)), (0, 0), (0, 0)]);
        links.tidy();
        links
    }
}

/// A grid's links: what they cost, those with an `id` by `id` and URI, and
/// the link the program has open as the grid has it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Links {
    /// What the links held cost: those the rows had when they were last
    /// counted (`sweep`), and every link made since.
    held: usize,
    /// The links with an `id`, by `id` and URI, until the rows are counted
    /// without them.
    ids: HashMap<(Arc<str>, Arc<str>), Hyperlink>,
    /// Links still to refuse before making room is tried again.
    stalled: u32,
    /// The link the program has open (`Pen`), as the grid has it.
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
    pub(crate) fn find(&self, uri: &Arc<str>, id: &Arc<str>) -> Option<&Hyperlink> {
        self.ids.get(&(Arc::clone(id), Arc::clone(uri)))
    }
    /// Whether a link of `uri` and `id` fits without making room.
    pub(crate) fn fits(&self, uri: &str, id: Option<&str>) -> bool {
        self.held.saturating_add(cost(uri, id)) <= LINK_BYTES
    }
    /// A new link, sharing the strings of the link opened; `None` if it
    /// does not fit.
    pub(crate) fn insert(
        &mut self,
        uri: &Arc<str>,
        id: Option<&Arc<str>>,
        key: u64,
    ) -> Option<Hyperlink> {
        let link = Hyperlink {
            key,
            uri: Arc::clone(uri),
            id: id.map(Arc::clone),
        };
        if !self.fits(uri, link.id()) {
            return None;
        }
        self.held = self.held.saturating_add(link.cost());
        if let Some(id) = id {
            self.ids
                .insert((Arc::clone(id), Arc::clone(uri)), link.clone());
        }
        Some(link)
    }
    /// Counts again the links `rows` have, each row with its index in a
    /// history of `len` rows, oldest first, or `None` for a row of the
    /// screen, after them; and how many of the oldest history rows must
    /// lose their links for the links left to cost at most half the bound,
    /// as many as there are if no number does. What the links left cost is
    /// held, and the links with an `id` that no row is left with go.
    pub(crate) fn sweep<'a>(
        &mut self,
        rows: impl Iterator<Item = (Option<usize>, &'a RowLinks)>,
        len: usize,
    ) -> usize {
        // Each link once, with the newest row that has it, a row of the
        // screen newer than any: by key, which links take as they open,
        // so mostly in order already.
        let mut seen: Vec<(u64, usize, usize)> = Vec::new();
        for (row, links) in rows {
            let row = row.unwrap_or(usize::MAX);
            seen.extend(links.links().map(|link| (link.key, row, link.cost())));
        }
        seen.sort_unstable();
        seen.dedup_by(|next, kept| {
            let same = next.0 == kept.0;
            if same {
                kept.1 = next.1;
            }
            same
        });
        // What each history row takes with it when it loses its links: the
        // links no newer row has.
        let mut live = 0usize;
        let mut leaving = vec![0usize; len];
        for &(_, row, cost) in &seen {
            live = live.saturating_add(cost);
            if let Some(leaves) = leaving.get_mut(row) {
                *leaves = leaves.saturating_add(cost);
            }
        }
        let mut lose = 0usize;
        for leaves in leaving {
            if live <= LINK_BYTES / 2 {
                break;
            }
            live = live.saturating_sub(leaves);
            lose = lose.saturating_add(1);
        }
        self.held = live;
        self.ids.retain(|_, link| {
            let at = seen.binary_search_by_key(&link.key, |&(key, _, _)| key);
            at.ok()
                .and_then(|at| seen.get(at))
                .is_some_and(|&(_, row, _)| row >= lose)
        });
        lose
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
        let live: HashMap<u64, usize> = live.map(|link| (link.key, link.cost())).collect();
        (self.held, live.values().sum())
    }
}

/// A link a program opened (OSC 8), which the cells printed take until it
/// is closed: its URI and `id`, and its key. Each grid it is printed in
/// makes its own link of it (`Links::open`).
#[derive(Clone, Debug)]
pub(crate) struct Pen {
    pub(crate) uri: Arc<str>,
    pub(crate) id: Option<Arc<str>>,
    pub(crate) key: u64,
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
    let split = payload.iter().position(|b| *b == b';')?;
    let params = payload.get(..split)?;
    let uri = payload.get(split.checked_add(1)?..)?;
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
    let uri = std::str::from_utf8(uri).ok()?;
    let id = match id {
        Some(id) => Some(std::str::from_utf8(id).ok()?),
        None => None,
    };
    Some((uri, id))
}

#[cfg(test)]
mod tests {
    use super::*;

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
