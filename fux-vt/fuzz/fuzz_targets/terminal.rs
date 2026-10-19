#![no_main]
//! The fast target: a screen of at most 16 by 24 and a few rows of history,
//! fed each operation whole, a byte at a time and in pieces as fux's pane
//! reads, the three compared and fux-vt's invariants checked after each
//! operation (`threats.md` section 6), the byte-at-a-time copy's row
//! versions after every byte. The `deep` target checks them so at large
//! sizes.
use fux_vt::{Feature, Identity, Options, Parser, Size};
use libfuzzer_sys::fuzz_target;
mod common;
use common::{
    Record, marks_sound, pick, places, prints_bounded, rows, until_stops, versions_follow,
};
#[path = "../../tests/corpus/graphemes.rs"]
mod graphemes;
#[path = "../../tests/corpus/invariants.rs"]
mod invariants;
#[cfg(feature = "checks")]
#[path = "../../tests/corpus/known.rs"]
mod known;

/// A size of 1 + r mod 16 rows by 1 + c mod 24 columns.
fn size(r: u8, c: u8) -> Option<Size> {
    let side = |n: u8, of: u8| u16::from(n.checked_rem(of).unwrap_or(0)).saturating_add(1);
    Size::new(side(r, 16), side(c, 24)).ok()
}

fuzz_target!(|data: &[u8]| {
    let (Some(&r), Some(&c), Some(&history)) = (data.first(), data.get(1), data.get(2)) else {
        return;
    };
    // Header bits above the history count opt into events (0x10), extended
    // replies (0x20), hyperlinks (0x40) and prompt marks (0x80); above the
    // row count, into reflow
    // (0x10), the kitty keyboard protocol (0x20), an identity (0x40) and
    // colour-scheme updates (0x80); above the column count, into pictures
    // (sixel, 0x20), setting reports (DECRQSS, 0x40) and rectangle
    // checksums (0x80).
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
        .set(Feature::Pictures, c & 0x20 != 0)
        .set(Feature::SettingReports, c & 0x40 != 0)
        .set(Feature::RectangleChecksums, c & 0x80 != 0);
    let Some(first) = size(r, c) else {
        return;
    };
    let budget = usize::from(history % 16).saturating_mul(64);
    let Ok(mut whole) = Parser::with_options(first, budget, options) else {
        return;
    };
    // What a program prints is recorded, as fux records it.
    whole.record_prints(true);
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
                let Some(size) = size(r, c) else {
                    break;
                };
                // r's high bits, when set, give history a new budget first.
                if r >= 0x10 {
                    let bytes = usize::from(r >> 4).saturating_mul(64);
                    for p in [&mut whole, &mut split, &mut chunked] {
                        p.set_history_bytes(bytes);
                    }
                }
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
                // Once a window's height and width: a window is the screen's.
                let _ = values.nth(1);
                let a = (
                    u16::from(values.next().unwrap_or_default()),
                    u16::from(values.next().unwrap_or_default()),
                );
                let b = (
                    u16::from(values.next().unwrap_or_default()),
                    u16::from(values.next().unwrap_or_default()),
                );
                let cells = usize::from(values.next().unwrap_or_default());
                let bytes = cells.saturating_mul(4);
                fn window(p: &Parser, offset: usize) -> fux_vt::Window<'_> {
                    let window = p.screen().window();
                    window.row(0).map_or(window, |top| top.up(offset).window())
                }
                let mark = whole.screen().mark();
                let result = window(&whole, offset).text(a, b, cells, bytes);
                assert_eq!(result, window(&split, offset).text(a, b, cells, bytes));
                if let Ok(text) = result {
                    assert!(text.len() <= bytes);
                }
                assert_eq!(mark, whole.screen().mark());
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
                    for n in picks.iter().take(count) {
                        if let Some(c) = pick(graphemes::CHARACTERS, usize::from(*n)) {
                            let mut buffer = [0; 4];
                            text.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
                        }
                    }
                    (count.saturating_add(1).min(input.len()), &text[..])
                } else {
                    let length = usize::from(operation).saturating_add(1).min(input.len());
                    (length, input.get(..length).unwrap_or_default())
                };
                let mut a = Record::default();
                let mut b = Record::default();
                let (before, at, mark) = (rows(&whole), places(&whole), whole.screen().mark());
                assert!(whole.process_with(bytes, &mut a).is_ok());
                versions_follow(&before, &whole);
                marks_sound(&at, mark, &whole);
                // A byte at a time, each byte's changes checked too: a row
                // changed and changed back within an operation still takes
                // a new version (the screen is small, its rows few).
                for byte in bytes {
                    let one = std::slice::from_ref(byte);
                    let before = rows(&split);
                    assert!(split.process_with(one, &mut b).is_ok());
                    versions_follow(&before, &split);
                }
                assert_eq!(a, b);
                let mut c = Record::default();
                let mut rest = bytes;
                let mut cut = bytes
                    .iter()
                    .fold(length, |h, b| h.wrapping_mul(31) ^ usize::from(*b));
                while !rest.is_empty() {
                    let size = (cut % 16).saturating_add(2);
                    cut = cut.rotate_right(4) ^ size;
                    let (piece, tail) = rest.split_at_checked(size).unwrap_or((rest, &[]));
                    until_stops(&mut chunked, piece, &mut c, |_| {});
                    rest = tail;
                }
                assert_eq!(a, c);
                input = input.get(length..).unwrap_or_default();
            }
        }
        invariants::check(&whole);
        invariants::same_feed(&whole, &split);
        invariants::same_feed(&whole, &chunked);
        for p in [&mut whole, &mut split, &mut chunked] {
            prints_bounded(p);
        }
    }
});
