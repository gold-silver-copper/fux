#![no_main]
use fux_vt::{
    Color, Event, Feature, Identity, OSC_PAYLOAD_LIMIT, Options, Parser, RowId, Sink, Size,
    Unhandled,
};
use libfuzzer_sys::fuzz_target;
#[path = "../../tests/corpus/graphemes.rs"]
mod graphemes;
#[path = "../../tests/corpus/invariants.rs"]
mod invariants;

/// Records replies and events so whole and byte-at-a-time processing can be
/// compared; every event payload must respect the OSC bound.
#[derive(Default, PartialEq, Debug)]
struct Record(Vec<Vec<u8>>);
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
                assert!(selection.len() + data.len() < OSC_PAYLOAD_LIMIT);
                entry.push(b'C');
                entry.extend_from_slice(selection);
                entry.push(0);
                entry.extend_from_slice(data);
            }
            _ => entry.push(b'?'),
        }
        assert!(entry.len() <= OSC_PAYLOAD_LIMIT + 2);
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
            _ => entry.push(b'?'),
        }
        self.0.push(entry);
    }
}

/// A colour as a number, for hashing.
fn color(c: Color) -> u32 {
    match c {
        Color::Idx(i) => 0x100 | u32::from(i),
        Color::Rgb(r, g, b) => 0x0100_0000 | u32::from_be_bytes([0, r, g, b]),
        // A kind fux-vt adds later hashes as the default until named here.
        Color::Default | _ => 0,
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

/// A hash of a row's cells: their text, halves, attributes and links.
/// Checked after every byte of byte-at-a-time processing, so it hashes
/// rather than copies: a collision could only hide a change, with odds of
/// 2^-64.
fn cells_hash(row: fux_vt::Row<'_>) -> u64 {
    let mut h = Fnv(0xcbf2_9ce4_8422_2325);
    for (col, cell) in row.cells().enumerate() {
        if let Some(link) = row.link(col) {
            h.bytes(link.uri().as_bytes());
            h.bytes(&link.key().to_le_bytes());
        }
        let a = cell.attributes();
        h.bytes(cell.contents().as_bytes());
        h.u32(color(a.foreground()));
        h.u32(color(a.background()));
        h.u32(color(a.underline_color()));
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
            .fold(a.blink() as u32, |n, f| (n << 1) | u32::from(*f));
        // Ends each cell, so text cannot run into the next cell's.
        h.u32(bits | 0x8000_0000);
    }
    h.0
}

/// Every retained row, sorted by identity: its version, wrap flag and
/// cells' hash.
fn rows(p: &Parser) -> Vec<(RowId, u64, bool, u64)> {
    let screen = p.screen();
    let retained = screen.history_len() + usize::from(screen.size().rows());
    let mut rows: Vec<_> = (0..retained)
        .filter_map(|i| screen.row_from_bottom(i))
        .map(|row| (row.id(), row.version(), row.wrapped(), cells_hash(row)))
        .collect();
    rows.sort_unstable_by_key(|r| r.0);
    rows
}

/// A row whose cells or wrap flag changed has a newer version: a change is
/// never missed.
fn versions_follow(before: &[(RowId, u64, bool, u64)], after: &Parser) {
    for (id, version, wrapped, cells) in rows(after) {
        if let Ok(at) = before.binary_search_by_key(&id, |r| r.0)
            && let Some((_, was, was_wrapped, was_cells)) = before.get(at)
            && (wrapped != *was_wrapped || cells != *was_cells)
        {
            assert!(version > *was, "{id:?} changed without a new version");
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let (Some(&r), Some(&c), Some(&history)) = (data.first(), data.get(1), data.get(2)) else {
        return;
    };
    // Header bits above the history count opt into events (0x10), extended
    // replies (0x20), hyperlinks (0x40) and prompt marks (0x80); above the
    // row count, into reflow
    // (0x10), the kitty keyboard protocol (0x20), an identity (0x40) and
    // colour-scheme updates (0x80); above the column count, into setting
    // reports (DECRQSS, 0x40) and rectangle checksums (0x80).
    // Each is fuzzed alone and with the others, alongside the default.
    let options = Options::new()
        .set(Feature::Events, history & 0x10 != 0)
        .set(Feature::ExtendedReplies, history & 0x20 != 0)
        .set(Feature::Hyperlinks, history & 0x40 != 0)
        .set(Feature::PromptMarks, history & 0x80 != 0)
        .set(Feature::Reflow, r & 0x10 != 0)
        .set(Feature::KittyKeyboard, r & 0x20 != 0)
        .with_identity((r & 0x40 != 0).then_some(Identity {
            name: "fuzz",
            version: "1.2.3",
        }))
        .set(Feature::ColorSchemeUpdates, r & 0x80 != 0)
        .set(Feature::SettingReports, c & 0x40 != 0)
        .set(Feature::RectangleChecksums, c & 0x80 != 0);
    let size = Size::new(1 + u16::from(r % 16), 1 + u16::from(c % 24)).expect("one at least");
    let Ok(mut whole) = Parser::with_options(size, usize::from(history % 16), options) else {
        return;
    };
    let mut split = whole.clone();
    // A third copy is fed each piece of output cut at sizes from 2 to 17,
    // chosen by the output itself: runs of text and clusters then break at
    // places neither whole nor byte-at-a-time processing puts a break.
    let mut chunked = whole.clone();
    let mut input = data.get(3..).unwrap_or_default();
    while let Some((&operation, tail)) = input.split_first() {
        input = tail;
        match operation {
            0xff => {
                let (Some(&r), Some(&c)) = (input.first(), input.get(1)) else {
                    break;
                };
                input = input.get(2..).unwrap_or_default();
                let size = Size::new(1 + u16::from(r % 16), 1 + u16::from(c % 24));
                let size = size.expect("one at least");
                // Resizing to the size it has changes nothing, not even marks.
                let unchanged = whole.screen().size() == size;
                let mark = whole.screen().mark();
                assert!(chunked.resize(size).is_ok());
                assert!(whole.resize(size).is_ok());
                if unchanged {
                    assert_eq!(whole.screen().mark(), mark);
                    assert!(!whole.screen().full_refresh_since(mark));
                }
                assert!(split.resize(size).is_ok());
            }
            0xfe => {
                let Some(parameters) = input.get(..8) else {
                    break;
                };
                input = input.get(8..).unwrap_or_default();
                let mut values = parameters.iter().copied();
                let offset = usize::from(values.next().unwrap_or_default());
                let height = u16::from(values.next().unwrap_or_default());
                let width = u16::from(values.next().unwrap_or_default());
                let a = (
                    u16::from(values.next().unwrap_or_default()),
                    u16::from(values.next().unwrap_or_default()),
                );
                let b = (
                    u16::from(values.next().unwrap_or_default()),
                    u16::from(values.next().unwrap_or_default()),
                );
                let cells = usize::from(values.next().unwrap_or_default());
                let bytes = cells * 4;
                let screen = whole.screen();
                let mark = screen.mark();
                let result = screen
                    .window(offset, height, width)
                    .text(a, b, cells, bytes);
                assert_eq!(
                    result,
                    split
                        .screen()
                        .window(offset, height, width)
                        .text(a, b, cells, bytes)
                );
                if let Ok(text) = result {
                    assert!(text.len() <= bytes);
                }
                assert_eq!(mark, screen.mark());
            }
            operation => {
                // 0xfd: a count, then a byte a character, each one of
                // `graphemes::CHARACTERS`, so clusters of every kind, and
                // longer than a cell holds inline, are made far more often
                // than random bytes make them. Otherwise, raw bytes.
                let mut text = Vec::new();
                let (length, bytes) = if operation == 0xfd {
                    let count = input.first().map_or(0, |n| usize::from(n % 64));
                    let picks = input.get(1..).unwrap_or_default();
                    for pick in picks.iter().take(count) {
                        let table = graphemes::CHARACTERS;
                        let c = table[usize::from(*pick) % table.len()];
                        let mut buffer = [0; 4];
                        text.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
                    }
                    ((count + 1).min(input.len()), &text[..])
                } else {
                    let length = (usize::from(operation) + 1).min(input.len());
                    (length, input.get(..length).unwrap_or_default())
                };
                let mut a = Record::default();
                let mut b = Record::default();
                let before = rows(&whole);
                assert!(whole.process_with(bytes, &mut a).is_ok());
                versions_follow(&before, &whole);
                for byte in bytes {
                    let before = rows(&split);
                    assert!(
                        split
                            .process_with(std::slice::from_ref(byte), &mut b)
                            .is_ok()
                    );
                    versions_follow(&before, &split);
                }
                assert_eq!(a, b);
                let mut c = Record::default();
                let mut rest = bytes;
                let mut cut = bytes
                    .iter()
                    .fold(length, |h, b| h.wrapping_mul(31) ^ usize::from(*b));
                while !rest.is_empty() {
                    let size = cut % 16 + 2;
                    cut = cut.rotate_right(4) ^ size;
                    let (piece, tail) = rest.split_at_checked(size).unwrap_or((rest, &[]));
                    assert!(chunked.process_with(piece, &mut c).is_ok());
                    rest = tail;
                }
                assert_eq!(a, c);
                input = input.get(length..).unwrap_or_default();
            }
        }
        invariants::check(&whole);
        invariants::equal(&whole, &split);
        invariants::equal(&whole, &chunked);
    }
});
