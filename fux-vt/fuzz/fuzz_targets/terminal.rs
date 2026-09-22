#![no_main]
use fux_vt::Parser;
use libfuzzer_sys::fuzz_target;
#[path = "../../tests/corpus/invariants.rs"]
mod invariants;

fuzz_target!(|data: &[u8]| {
    let (Some(&r), Some(&c), Some(&history)) = (data.first(), data.get(1), data.get(2)) else {
        return;
    };
    let Ok(mut whole) = Parser::new(
        1 + u16::from(r % 16),
        1 + u16::from(c % 24),
        usize::from(history % 16),
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
                let mut a = Vec::new();
                let mut b = Vec::new();
                assert!(
                    whole
                        .process_with_replies(bytes, |r| a.push(r.to_vec()))
                        .is_ok()
                );
                for byte in bytes {
                    assert!(
                        split
                            .process_with_replies(std::slice::from_ref(byte), |r| b
                                .push(r.to_vec()))
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
