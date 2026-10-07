//! Hyperlinks (OSC 8, `references/modern/osc8_hyperlinks.md`): the links a
//! grid's cells point to, each held once, and the cells' links, held per row
//! beside the cells for the rows that have any.
//!
//! A cell's link is a `u16` in its row's array, 0 for none and otherwise one
//! more than the link's place in the table. Only a cell with contents has
//! one: an erased cell keeps whatever number it had, unread, until a glyph
//! is printed there, which always writes its link; so erasing part of a
//! row, which blanks cells, never touches its array. A whole row erased
//! drops its array, as nothing in it could be read again (`Grid::erase`).
//! The second half of a wide glyph has its first half's link.
//!
//! The table counts the numbers in the arrays (an erased cell's too), so the
//! links no row has are found without reading the rows: every change to an
//! array holds and releases the numbers it writes and drops.

use std::collections::HashMap;
use std::sync::Arc;

/// The longest URI kept, as VTE and iTerm2 bound it (the spec's "Length
/// limits"). A longer one opens no link.
pub const URI_LIMIT: usize = 2083;
/// The longest `id` kept, as VTE bounds it (the spec's "Length limits"). A
/// link with a longer one is not opened.
pub const ID_LIMIT: usize = 250;
/// The most links a grid holds at once: a cell's link is a `u16`, 0 for none.
pub(crate) const LINK_COUNT: usize = 65_535;
/// The most bytes of URIs and ids a grid holds at once, each link counted
/// with `ENTRY_COST` more for what holds it.
pub(crate) const LINK_BYTES: usize = 4 << 20;
/// What a link costs beyond its URI and id: its entry, and its place in the
/// map of ids.
const ENTRY_COST: usize = 64;
/// After making room fails (every link left is on the screen), how many
/// more links are refused before trying again: the links and the history's
/// rows are walked at most once in this many links.
const STALL: u32 = 256;

/// A cell's hyperlink: where it points, the program's `id` for it, if it
/// gave one, and a number that tells links apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hyperlink<'a> {
    uri: &'a str,
    id: Option<&'a str>,
    key: u64,
}

impl<'a> Hyperlink<'a> {
    /// The URI, as the program sent it: at most [`URI_LIMIT`] bytes, each a
    /// printable ASCII character.
    pub fn uri(&self) -> &'a str {
        self.uri
    }
    /// The `id` parameter the program gave the link, if it gave a nonempty
    /// one: at most [`ID_LIMIT`] printable ASCII bytes.
    pub fn id(&self) -> Option<&'a str> {
        self.id
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
}

/// A link as a grid holds it, with how many cells have its number.
#[derive(Clone, Debug)]
struct Entry {
    uri: Arc<str>,
    id: Option<Arc<str>>,
    key: u64,
    cells: u32,
}

impl Entry {
    fn cost(uri: &str, id: Option<&str>) -> usize {
        uri.len()
            .saturating_add(id.map_or(0, str::len))
            .saturating_add(ENTRY_COST)
    }
}

/// A grid's links: each once, found by its number, and by `id` and URI for
/// the links that have an `id`; with how many cells have each, so that a
/// link no cell has is found without reading the cells.
#[derive(Clone, Debug, Default)]
pub(crate) struct Links {
    /// Link `n` is at `n - 1`; a freed link leaves `None`.
    entries: Vec<Option<Entry>>,
    /// Numbers of freed links, to give again.
    free: Vec<u16>,
    /// The links with an `id`, by `id` and URI.
    ids: HashMap<(Arc<str>, Arc<str>), u16>,
    /// What the links held cost (`Entry::cost`).
    bytes: usize,
    /// How many of them some cell has, and what those cost.
    used: (usize, usize),
    /// Links still to refuse before making room is tried again.
    stalled: u32,
}

impl Links {
    /// Link `n`, if it is held.
    pub(crate) fn get(&self, n: u16) -> Option<Hyperlink<'_>> {
        let entry = self.entry(n)?;
        Some(Hyperlink {
            uri: &entry.uri,
            id: entry.id.as_deref(),
            key: entry.key,
        })
    }
    fn entry(&self, n: u16) -> Option<&Entry> {
        self.entries.get(usize::from(n).checked_sub(1)?)?.as_ref()
    }
    /// How many links are held.
    pub(crate) fn len(&self) -> usize {
        self.entries.len().saturating_sub(self.free.len())
    }
    /// The number of the held link with this `id` and URI.
    pub(crate) fn find(&self, uri: &Arc<str>, id: &Arc<str>) -> Option<u16> {
        self.ids.get(&(Arc::clone(id), Arc::clone(uri))).copied()
    }
    /// Whether a link of `uri` and `id` fits without making room.
    pub(crate) fn fits(&self, uri: &str, id: Option<&str>) -> bool {
        self.len() < LINK_COUNT && self.bytes.saturating_add(Entry::cost(uri, id)) <= LINK_BYTES
    }
    /// Whether the links some cell has are at most half the bounds, as
    /// making room leaves them.
    pub(crate) fn used_within_half(&self) -> bool {
        let (count, bytes) = self.used;
        count <= LINK_COUNT / 2 && bytes <= LINK_BYTES / 2
    }
    /// Holds a new link, sharing the strings of the link opened, which no
    /// cell has yet; its number, or `None` if it does not fit.
    pub(crate) fn insert(
        &mut self,
        uri: &Arc<str>,
        id: Option<&Arc<str>>,
        key: u64,
    ) -> Option<u16> {
        let cost = Entry::cost(uri, id.map(|id| &**id));
        if !self.fits(uri, id.map(|id| &**id)) {
            return None;
        }
        let entry = Entry {
            uri: Arc::clone(uri),
            id: id.map(Arc::clone),
            key,
            cells: 0,
        };
        let n = match self.free.pop() {
            Some(n) => {
                *self.entries.get_mut(usize::from(n).checked_sub(1)?)? = Some(entry);
                n
            }
            None => {
                self.entries.push(Some(entry));
                u16::try_from(self.entries.len()).ok()?
            }
        };
        if let Some(id) = id {
            self.ids.insert((Arc::clone(id), Arc::clone(uri)), n);
        }
        self.bytes = self.bytes.saturating_add(cost);
        Some(n)
    }
    /// `cells` more cells have link `n` (0, none, is no link).
    pub(crate) fn hold(&mut self, n: u16, cells: u32) {
        let Some(Some(entry)) = usize::from(n)
            .checked_sub(1)
            .and_then(|i| self.entries.get_mut(i))
        else {
            return;
        };
        if entry.cells == 0 && cells > 0 {
            let cost = Entry::cost(&entry.uri, entry.id.as_deref());
            self.used = (
                self.used.0.saturating_add(1),
                self.used.1.saturating_add(cost),
            );
        }
        entry.cells = entry.cells.saturating_add(cells);
    }
    /// `cells` cells no longer have link `n`.
    pub(crate) fn release(&mut self, n: u16, cells: u32) {
        let Some(Some(entry)) = usize::from(n)
            .checked_sub(1)
            .and_then(|i| self.entries.get_mut(i))
        else {
            return;
        };
        let left = entry.cells.saturating_sub(cells);
        if entry.cells > 0 && left == 0 {
            let cost = Entry::cost(&entry.uri, entry.id.as_deref());
            self.used = (
                self.used.0.saturating_sub(1),
                self.used.1.saturating_sub(cost),
            );
        }
        entry.cells = left;
    }
    /// The cells `row` no longer have their links: counted a run of one
    /// link at a time, as a link's cells are side by side.
    pub(crate) fn release_all(&mut self, row: &[u16]) {
        let mut rest = row;
        while let Some(&n) = rest.first() {
            let run = rest.iter().take_while(|m| **m == n).count();
            if n != 0 {
                self.release(n, u32::try_from(run).unwrap_or(u32::MAX));
            }
            rest = rest.get(run..).unwrap_or_default();
        }
    }
    /// Counts the cells of each link again, from every row's links: after
    /// a resize, which copies rows' links rather than moving them.
    pub(crate) fn recount<'a>(&mut self, rows: impl Iterator<Item = &'a [u16]>) {
        for entry in self.entries.iter_mut().flatten() {
            entry.cells = 0;
        }
        self.used = (0, 0);
        for row in rows {
            for &n in row {
                self.hold(n, 1);
            }
        }
    }
    /// Frees the links no cell has, but `keep`.
    pub(crate) fn free_unused(&mut self, keep: &[u16]) {
        for (n, slot) in (1..=u16::MAX).zip(self.entries.iter_mut()) {
            if keep.contains(&n) || slot.as_ref().is_none_or(|e| e.cells > 0) {
                continue;
            }
            let Some(entry) = slot.take() else {
                continue;
            };
            self.bytes = self
                .bytes
                .saturating_sub(Entry::cost(&entry.uri, entry.id.as_deref()));
            if let Some(id) = entry.id {
                self.ids.remove(&(id, entry.uri));
            }
        }
        // Freed numbers at the end are dropped, the rest kept for reuse.
        while self.entries.last().is_some_and(Option::is_none) {
            self.entries.pop();
        }
        self.free.clear();
        let freed = (1..=u16::MAX)
            .zip(&self.entries)
            .filter(|(_, e)| e.is_none());
        self.free.extend(freed.map(|(n, _)| n));
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
    /// How many cells have each link, by number: for tests that count them
    /// from the cells.
    #[cfg(test)]
    pub(crate) fn counts(&self) -> Vec<u32> {
        self.entries
            .iter()
            .map(|e| e.as_ref().map_or(0, |e| e.cells))
            .collect()
    }
}

/// A link a program opened (OSC 8), which the cells printed take until it
/// is closed: its URI and `id`, its key, and its number in each grid it was
/// printed in.
#[derive(Clone, Debug)]
pub(crate) struct Pen {
    pub(crate) uri: Arc<str>,
    pub(crate) id: Option<Arc<str>>,
    pub(crate) key: u64,
    /// Its number in the primary grid and in the alternate.
    pub(crate) held: [Held; 2],
}

/// Where a pen's link is in one grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Held {
    /// Not printed there yet.
    Pending,
    /// Held as this number.
    At(u16),
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

    fn s(text: &str) -> Arc<str> {
        Arc::from(text)
    }

    #[test]
    fn links_are_held_within_their_bounds_and_freed() {
        let mut links = Links::default();
        assert_eq!(links.insert(&s("a"), None, 1), Some(1));
        assert_eq!(links.insert(&s("b"), Some(&s("x")), 2), Some(2));
        assert_eq!(links.find(&s("b"), &s("x")), Some(2));
        assert_eq!(links.find(&s("a"), &s("x")), None);
        assert_eq!(
            links.get(2).map(|l| (l.uri(), l.id(), l.key())),
            Some(("b", Some("x"), 2))
        );
        // Held by cells, a link stays; held by none, it goes, unless kept.
        links.hold(2, 3);
        links.release(2, 1);
        links.hold(0, 5);
        links.release_all(&[2, 0, 0]);
        links.free_unused(&[]);
        assert_eq!(links.get(1), None);
        assert_eq!(links.len(), 1);
        assert_eq!(links.counts(), [0, 1]);
        assert_eq!(links.used.0, 1);
        // A freed number is given again.
        assert_eq!(links.insert(&s("c"), None, 3), Some(1));
        links.release_all(&[2, 2]);
        assert_eq!(links.used, (0, 0));
        links.free_unused(&[1]);
        assert_eq!(links.find(&s("b"), &s("x")), None);
        assert_eq!(links.counts(), [0]);
        links.recount([&[1u16, 1, 0][..], &[1]].into_iter());
        assert_eq!(links.counts(), [3]);
        assert_eq!(links.used.0, 1);
        // The bytes bound: links of the longest URI up to it.
        let uri: Arc<str> = std::iter::repeat_n("u", URI_LIMIT)
            .collect::<String>()
            .into();
        let mut n = 0;
        while links.insert(&uri, None, 9).is_some() {
            n += 1;
        }
        assert!(n > 0 && links.bytes <= LINK_BYTES);
        assert!(!links.fits(&uri, None));
        assert!(
            links.fits("short", None) == (links.bytes + Entry::cost("short", None) <= LINK_BYTES)
        );
    }
}
