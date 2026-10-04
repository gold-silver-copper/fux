//! The working tree's fux-vt beside the pinned commit's (`diff/oracle`),
//! fed the same random bytes, resizes, frames and copies: everything its
//! API shows must be equal after every step.
//!
//! Input: a case as `fux-vt-oracle --replay` reads it (text starting
//! `size `), which is how the oracle's own random cases seed the fuzzer
//! (`fux-vt-oracle --seeds DIR`); or five header bytes, then operations.
//!
//! - Header: rows (1 to 16), columns (1 to 24), history (one of ten sizes,
//!   0 to 100, so it fills), then two bytes of options: the first's bits
//!   turn on events, extended replies, mode reports, in-band resize, size
//!   reports, colour-scheme updates, the kitty keyboard protocol and
//!   reflow; the second's, hyperlinks, prompt marks, rectangle checksums,
//!   setting reports, (two bits) an identity, and the palette.
//! - `ff rows cols`: a resize, to 1 to 40 rows and 1 to 100 columns.
//! - `fb n`: a resize fux-vt refuses for capacity, to 65,535 rows and
//!   1,100 columns or more (`n` picks), or to 65,535 by 65,535.
//! - `fe` and eight bytes: a window (offset, rows, columns), two corners
//!   and the copy's cell and byte limits.
//! - `fd n` and n bytes: output through `process_until_frame`.
//! - `fc n` and n bytes: a character of fux-vt's grapheme table for each,
//!   so clusters of every kind come often.
//! - Any other byte `b`: the next `b + 1` bytes as output.
#![no_main]
use fux_vt_oracle::graphemes::CHARACTERS;
use fux_vt_oracle::model::{IDENTITIES, Selection, Setup};
use fux_vt_oracle::{Case, Step, check};
use libfuzzer_sys::fuzz_target;

const HISTORIES: [usize; 10] = [0, 1, 2, 3, 5, 8, 13, 20, 40, 100];

fn bit(byte: u8, n: u8) -> bool {
    byte.checked_shr(u32::from(n)).is_some_and(|b| b & 1 == 1)
}

/// The most bytes of output, and steps, a case given as text may have,
/// so each run stays quick.
const TEXT_BYTES: usize = 64 * 1024;
const TEXT_STEPS: usize = 300;

/// The case the bytes describe.
fn case(data: &[u8]) -> Option<Case> {
    if data.starts_with(b"size ") {
        let case = Case::from_text(std::str::from_utf8(data).ok()?).ok()?;
        return (case.is_safe() && case.bytes() <= TEXT_BYTES && case.steps.len() <= TEXT_STEPS)
            .then_some(case);
    }
    let (header, mut input) = data.split_at_checked(5)?;
    let [rows, cols, history, a, b] = <[u8; 5]>::try_from(header).ok()?;
    let setup = Setup {
        events: bit(a, 0),
        extended_replies: bit(a, 1),
        mode_reports: bit(a, 2),
        in_band_resize: bit(a, 3),
        size_reports: bit(a, 4),
        color_scheme_updates: bit(a, 5),
        kitty_keyboard: bit(a, 6),
        reflow: bit(a, 7),
        hyperlinks: bit(b, 0),
        prompt_marks: bit(b, 1),
        rectangle_checksums: bit(b, 2),
        setting_reports: bit(b, 3),
        palette: bit(b, 6),
        identity: match (b >> 4) & 3 {
            0 => None,
            n => IDENTITIES.get(usize::from(n.saturating_sub(1))).copied(),
        },
    };
    let mut case = Case {
        rows: 1 + u16::from(rows % 16),
        cols: 1 + u16::from(cols % 24),
        history: HISTORIES
            .get(usize::from(history % 10))
            .copied()
            .unwrap_or(0),
        setup,
        steps: Vec::new(),
    };
    while let Some((&operation, rest)) = input.split_first() {
        input = rest;
        match operation {
            0xff => {
                let (&[r, c], rest) = input.split_first_chunk::<2>()?;
                input = rest;
                case.steps
                    .push(Step::Resize(1 + u16::from(r % 40), 1 + u16::from(c % 100)));
            }
            0xfb => {
                let (&n, rest) = input.split_first()?;
                input = rest;
                let cols = if n == 0xff {
                    u16::MAX
                } else {
                    1_100u16.saturating_add(u16::from(n))
                };
                case.steps.push(Step::Resize(u16::MAX, cols));
            }
            0xfe => {
                let (&[offset, h, w, y0, x0, y1, x1, limit], rest) =
                    input.split_first_chunk::<8>()?;
                input = rest;
                let cells = usize::from(limit);
                case.steps.push(Step::Copy(Selection {
                    offset: usize::from(offset),
                    rows: u16::from(h % 32),
                    cols: u16::from(w),
                    from: (u16::from(y0 % 32), u16::from(x0)),
                    to: (u16::from(y1 % 32), u16::from(x1)),
                    max_cells: if limit == 0xff { usize::MAX } else { cells },
                    max_bytes: if limit == 0xff {
                        usize::MAX
                    } else {
                        cells.saturating_mul(4)
                    },
                }));
            }
            0xfd | 0xfc => {
                let (&n, rest) = input.split_first()?;
                let n = usize::from(n % 64) + 1;
                let (bytes, rest) = rest.split_at_checked(n).unwrap_or((rest, &[]));
                input = rest;
                if operation == 0xfd {
                    case.steps.push(Step::Frame(bytes.to_vec()));
                } else {
                    let text: String = bytes
                        .iter()
                        .filter_map(|b| {
                            CHARACTERS.get(usize::from(*b).checked_rem(CHARACTERS.len())?)
                        })
                        .collect();
                    case.steps.push(Step::Output(text.into_bytes()));
                }
            }
            n => {
                let length = usize::from(n) + 1;
                let (bytes, rest) = input.split_at_checked(length).unwrap_or((input, &[]));
                input = rest;
                case.steps.push(Step::Output(bytes.to_vec()));
            }
        }
    }
    Some(case)
}

fuzz_target!(|data: &[u8]| {
    let Some(case) = case(data) else {
        return;
    };
    let result = check(&case)
        .map(|_| ())
        .map_err(|d| format!("{d}\n\n{}", case.to_text()));
    assert_eq!(result, Ok(()));
});
