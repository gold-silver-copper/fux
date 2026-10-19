//! What the `terminal` and `deep` targets share: the sink that records what
//! a parser says, the hashes of rows a change of version is checked
//! against, and feeding as fux's pane does.

use fux_vt::{Color, Event, OSC_PAYLOAD_LIMIT, Parser, Rgb, RowId, Sink, Stop, Unhandled};

/// Records replies and events so whole and split processing can be
/// compared; every event payload must respect the OSC bound.
#[derive(Default, PartialEq, Debug)]
pub struct Record(pub Vec<Vec<u8>>);

impl Sink for Record {
    fn reply(&mut self, bytes: &[u8]) {
        self.0.push(bytes.to_vec());
    }
    fn event(&mut self, event: Event<'_>) {
        let mut entry = vec![0xff];
        match event {
            Event::Title(t) => {
                entry.push(b'T');
                entry.extend_from_slice(t);
            }
            Event::IconName(t) => {
                entry.push(b'I');
                entry.extend_from_slice(t);
            }
            Event::Bell => entry.push(b'B'),
            Event::ColorQuery { number, bel } => {
                entry.push(b'Q');
                entry.push(number);
                entry.push(u8::from(bel));
            }
            Event::Clipboard { selection, data } => {
                assert!(selection.len().saturating_add(data.len()) < OSC_PAYLOAD_LIMIT);
                entry.push(b'C');
                entry.extend_from_slice(selection);
                entry.push(0);
                entry.extend_from_slice(data);
            }
            Event::Notification(text) => {
                assert!(text.len() < OSC_PAYLOAD_LIMIT);
                entry.push(b'N');
                entry.extend_from_slice(text);
            }
        }
        assert!(entry.len() <= OSC_PAYLOAD_LIMIT.saturating_add(2));
        self.0.push(entry);
    }
    fn unhandled(&mut self, sequence: Unhandled<'_>) {
        let mut entry = vec![0xfe];
        match sequence {
            Unhandled::Csi {
                params,
                intermediates,
                action,
            } => {
                entry.push(b'C');
                entry.extend_from_slice(intermediates);
                for group in params.groups() {
                    for value in group {
                        entry.extend_from_slice(&value.to_le_bytes());
                    }
                    entry.push(b';');
                }
                entry.push(action);
            }
            Unhandled::Escape {
                intermediates,
                action,
            } => {
                entry.push(b'E');
                entry.extend_from_slice(intermediates);
                entry.push(action);
            }
        }
        self.0.push(entry);
    }
}

/// The entry of `table` that `n` picks, counting round it.
pub fn pick<T: Copy>(table: &[T], n: usize) -> Option<T> {
    table.get(n.checked_rem(table.len())?).copied()
}

/// A colour as a number, for hashing.
fn color(c: Color) -> u32 {
    match c {
        Color::Idx(i) => 0x100 | u32::from(i),
        Color::Rgb(Rgb { r, g, b }) => 0x0100_0000 | u32::from_be_bytes([0, r, g, b]),
        Color::Default => 0,
    }
}

/// FNV-1a, folded in a byte at a time: cheap enough to run on every row
/// after every byte of input, where SipHash and a hash map dominated.
struct Fnv(u64);

impl Fnv {
    fn bytes(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    fn u32(&mut self, n: u32) {
        self.bytes(&n.to_le_bytes());
    }
}

/// A hash of a row's cells: their text, halves, attributes and links. A
/// collision could only hide a change, with odds of 2^-64.
fn cells_hash(row: fux_vt::Row<'_>) -> u64 {
    let mut h = Fnv(0xcbf2_9ce4_8422_2325);
    let links = row.has_links();
    for (col, cell) in row.cells().enumerate() {
        if let Some(link) = links.then(|| row.link(col)).flatten() {
            h.bytes(link.uri().as_bytes());
            h.bytes(&link.key().to_le_bytes());
        }
        let a = cell.attributes();
        h.bytes(cell.contents().as_bytes());
        h.u32(color(a.foreground()));
        h.u32(color(a.background()));
        h.u32(color(a.underline_color()));
        let blink = match a.blink() {
            fux_vt::Blink::None => 0,
            fux_vt::Blink::Slow => 1,
            fux_vt::Blink::Rapid => 2,
        };
        let flags = [
            cell.is_wide(),
            cell.is_wide_continuation(),
            a.bold(),
            a.dim(),
            a.italic(),
            a.underline(),
            a.inverse(),
            a.hidden(),
            a.strikeout(),
        ];
        let bits = flags
            .iter()
            .fold(blink, |n: u32, f| (n << 1) | u32::from(*f));
        // Ends each cell, so text cannot run into the next cell's.
        h.u32(bits | 0x8000_0000);
    }
    h.0
}

/// A retained row: its identity, version, wrap flag, prompt mark and cells'
/// hash.
pub type Kept = (RowId, u64, bool, bool, u64);

/// Every retained row of the screen in use, top first, each hashed once,
/// and how many of them are history.
pub struct Rows {
    all: Vec<Kept>,
    history: usize,
}

impl Rows {
    pub fn of(p: &Parser) -> Self {
        let screen = p.screen();
        let all = (screen.rows())
            .map(|row| {
                let (id, version, wrapped) = (row.id(), row.version(), row.wrapped());
                (id, version, wrapped, row.starts_prompt(), cells_hash(row))
            })
            .collect();
        Self {
            all,
            history: screen.history_len(),
        }
    }

    /// The screen's rows by place, top first, as fux's painter reads them
    /// (`src/pane.rs`, `(y, row)`).
    fn places(&self) -> &[Kept] {
        self.all.get(self.history..).unwrap_or_default()
    }
}

/// A row whose cells, wrap flag or prompt mark changed has a newer version:
/// a change is never missed (`threats.md` section 6, 28).
pub fn versions_follow(before: &Rows, after: &Rows) {
    let mut before = before.all.clone();
    before.sort_unstable_by_key(|r| r.0);
    for (id, version, wrapped, prompt, cells) in &after.all {
        if let Ok(at) = before.binary_search_by_key(id, |r| r.0)
            && let Some((_, was, was_wrapped, was_prompt, was_cells)) = before.get(at)
            && (wrapped != was_wrapped || prompt != was_prompt || cells != was_cells)
        {
            assert!(version > was, "{id:?} changed without a new version");
        }
    }
}

/// The change marks are sound by place (`threats.md` section 6, 28): read
/// from `mark`, taken when the screen's rows were `before`, a reader of
/// `after`, whose rows are `now`, is told to redraw everything, or
/// `dirty_live_rows_since` names every place whose row, cells, wrap or
/// prompt mark changed, as fux redraws.
pub fn marks_sound(before: &Rows, mark: fux_vt::Mark, after: &Parser, now: &Rows) {
    let screen = after.screen();
    if screen.full_refresh_since(mark) {
        return;
    }
    let (now, before) = (now.places(), before.places());
    assert_eq!(
        now.len(),
        before.len(),
        "the screen's height changed with no full refresh"
    );
    let dirty: Vec<u16> = screen.dirty_live_rows_since(mark).map(|(y, _)| y).collect();
    for ((y, now), was) in (0u16..).zip(now).zip(before) {
        let (id, _, wrapped, prompt, cells) = now;
        let (was_id, _, was_wrapped, was_prompt, was_cells) = was;
        if (id, wrapped, prompt, cells) != (was_id, was_wrapped, was_prompt, was_cells) {
            assert!(screen.changed_since(mark), "place {y} changed, unmarked");
            assert!(
                dirty.contains(&y),
                "place {y} changed, and no reader is told"
            );
        }
    }
}

/// What `Prints` kept since the last take is within its caps: 4 bytes a
/// screen cell of text (16 KiB at least) and 4,096 runs (threats.md B10).
pub fn prints_bounded(p: &mut Parser) {
    let (rows, cols) = p.screen().size().into();
    let cells = usize::from(rows).saturating_mul(usize::from(cols));
    let prints = p.take_prints();
    let text = prints.iter().map(|print| print.text.len()).sum::<usize>();
    assert!(
        text <= cells.saturating_mul(4).max(16 << 10),
        "{text} bytes of prints"
    );
    assert!(prints.iter().count() <= 4096);
}

/// Feeds `piece` to `p` by `process_until_stop`, given again from each
/// stop, as fux's pane does; at each frame begun, `FrameScan` must find
/// the frame's end where the parser then ends it, if it ends in `piece`
/// (threats.md B19, PAR-1's agreement). `each` is called after every call,
/// with the bytes it took.
pub fn until_stops(p: &mut Parser, piece: &[u8], sink: &mut Record, mut each: impl FnMut(usize)) {
    let (mut left, mut frame) = (piece, None::<(Option<usize>, usize)>);
    while !left.is_empty() {
        let stop = p.process_until_stop(left, sink);
        assert!(stop.is_ok());
        let Ok(Some((taken, why))) = stop else {
            each(left.len());
            // The rest was read: a frame begun in it doesn't end there.
            if let Some((end, _)) = frame {
                assert_eq!(end, None, "a frame scan ended where the parser did not");
            }
            return;
        };
        each(taken);
        assert!(taken > 0 && taken <= left.len());
        left = left.get(taken..).unwrap_or_default();
        if let Some((end, read)) = &mut frame {
            *read = read.saturating_add(taken);
            if why == Stop::Ended {
                assert_eq!(
                    *end,
                    Some(*read),
                    "the parser ended a frame the scan did not"
                );
                frame = None;
            }
        } else if why == Stop::Begun {
            frame = Some((p.frame_scan().end(left), 0));
        }
    }
    if let Some((end, read)) = frame {
        assert!(
            end.is_none_or(|end| end > read),
            "the scan ended the frame early"
        );
    }
}
