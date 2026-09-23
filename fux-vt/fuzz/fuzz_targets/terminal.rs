#![no_main]
use fux_vt::{Event, OSC_PAYLOAD_LIMIT, Options, Parser, Sink};
use libfuzzer_sys::fuzz_target;
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
}

fuzz_target!(|data: &[u8]| {
    let (Some(&r), Some(&c), Some(&history)) = (data.first(), data.get(1), data.get(2)) else {
        return;
    };
    // Header bits above the history count opt into events (0x10) and extended
    // replies (0x20), so the opt-in paths are fuzzed alongside the default.
    let options = Options {
        events: history & 0x10 != 0,
        extended_replies: history & 0x20 != 0,
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
                let length = (usize::from(operation) + 1).min(input.len());
                let bytes = input.get(..length).unwrap_or_default();
                let mut a = Record::default();
                let mut b = Record::default();
                assert!(whole.process_with(bytes, &mut a).is_ok());
                for byte in bytes {
                    assert!(
                        split
                            .process_with(std::slice::from_ref(byte), &mut b)
                            .is_ok()
                    );
                }
                assert_eq!(a, b);
                input = input.get(length..).unwrap_or_default();
            }
        }
        invariants::check(&whole);
        invariants::equal(&whole, &split);
    }
});
