#![no_main]
use fux_vt::{Cells, Event, Identity, OSC_PAYLOAD_LIMIT, Options, Parser, RowId, Sink, Unhandled};
use libfuzzer_sys::fuzz_target;
use std::collections::HashMap;
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

/// Every retained row by identity: its version, wrap flag and cells.
fn rows(p: &Parser) -> HashMap<RowId, (u64, bool, Cells)> {
    let screen = p.screen();
    let retained = screen.history_len() + usize::from(screen.size().0);
    (0..retained)
        .filter_map(|i| screen.row_from_bottom(i))
        .map(|row| (row.id, (row.version, row.wrapped, row.cells().collect())))
        .collect()
}

/// A row whose cells or wrap flag changed has a newer version: a change is
/// never missed.
fn versions_follow(before: &HashMap<RowId, (u64, bool, Cells)>, after: &Parser) {
    for (id, (version, wrapped, cells)) in rows(after) {
        if let Some((was, was_wrapped, was_cells)) = before.get(&id)
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
    // Header bits above the history count opt into events (0x10) and
    // extended replies (0x20); above the row count, into reflow (0x10), the
    // kitty keyboard protocol (0x20) and an identity (0x40). Each is fuzzed
    // alone and with the others, alongside the default.
    let options = Options {
        events: history & 0x10 != 0,
        extended_replies: history & 0x20 != 0,
        reflow: r & 0x10 != 0,
        kitty_keyboard: r & 0x20 != 0,
        identity: (r & 0x40 != 0).then_some(Identity {
            name: "fuzz",
            version: "1.2.3",
        }),
    };
    let Ok(mut whole) = Parser::with_options(
        1 + u16::from(r % 16),
        1 + u16::from(c % 24),
        usize::from(history % 16),
        options,
    ) else {
        return;
    };
    let mut split = whole.clone();
    let mut input = data.get(3..).unwrap_or_default();
    while let Some((&operation, tail)) = input.split_first() {
        input = tail;
        match operation {
            0xff => {
                let (Some(&r), Some(&c)) = (input.first(), input.get(1)) else {
                    break;
                };
                input = input.get(2..).unwrap_or_default();
                assert!(
                    whole
                        .resize(1 + u16::from(r % 16), 1 + u16::from(c % 24))
                        .is_ok()
                );
                assert!(
                    split
                        .resize(1 + u16::from(r % 16), 1 + u16::from(c % 24))
                        .is_ok()
                );
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
                input = input.get(length..).unwrap_or_default();
            }
        }
        invariants::check(&whole);
        invariants::equal(&whole, &split);
    }
});
