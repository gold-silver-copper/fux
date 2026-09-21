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
                let screen = whole.screen();
                let window = screen.window(usize::MAX, screen.size().0, screen.size().1);
                let result = window.text((0, 0), (window.rows - 1, window.cols - 1), 384, 512);
                if let Ok(text) = result {
                    assert!(text.len() <= 512);
                }
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
