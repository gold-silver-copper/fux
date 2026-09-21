//! Temporary compatibility scaffolding. Remove only after the fixture/invariant
//! mapping and minimized divergence inventory have permanent coverage.
use fux_vt::{Cell, Color, Error, Parser, Screen};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[derive(Default)]
struct Replies(Vec<Vec<u8>>);
impl vt100::Callbacks for Replies {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        if i1.is_some() || i2.is_some() {
            return;
        }
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        let reply = match (c, first) {
            ('n', 5) => b"\x1b[0n".to_vec(),
            ('n', 6) => {
                let (row, col) = screen.cursor_position();
                format!("\x1b[{};{}R", u32::from(row) + 1, u32::from(col) + 1).into_bytes()
            }
            ('c', 0) => b"\x1b[?1;2c".to_vec(),
            _ => return,
        };
        self.0.push(reply);
    }
}
fn colour(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Default,
        vt100::Color::Idx(i) => Color::Idx(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}
fn compare_cell(ours: &Cell, reference: &vt100::Cell, label: &str) {
    assert_eq!(ours.contents(), reference.contents(), "contents {label}");
    assert_eq!(ours.is_wide(), reference.is_wide(), "wide {label}");
    assert_eq!(
        ours.is_wide_continuation(),
        reference.is_wide_continuation(),
        "continuation {label}"
    );
    assert_eq!(ours.fgcolor(), colour(reference.fgcolor()), "fg {label}");
    assert_eq!(ours.bgcolor(), colour(reference.bgcolor()), "bg {label}");
    assert_eq!(
        (
            ours.bold(),
            ours.dim(),
            ours.italic(),
            ours.underline(),
            ours.inverse()
        ),
        (
            reference.bold(),
            reference.dim(),
            reference.italic(),
            reference.underline(),
            reference.inverse()
        ),
        "flags {label}"
    );
}
fn compare(ours: &Screen, reference: &mut vt100::Screen, label: &str) -> Result {
    let (rows, cols) = ours.size();
    assert_eq!((rows, cols), reference.size(), "size {label}");
    assert_eq!(
        ours.cursor_position(),
        reference.cursor_position(),
        "cursor {label}"
    );
    assert_eq!(
        (
            ours.hide_cursor(),
            ours.application_cursor(),
            ours.bracketed_paste(),
            ours.alternate_screen()
        ),
        (
            reference.hide_cursor(),
            reference.application_cursor(),
            reference.bracketed_paste(),
            reference.alternate_screen()
        ),
        "modes {label}"
    );
    assert_eq!(
        format!("{:?}", ours.mouse_protocol_mode()),
        format!("{:?}", reference.mouse_protocol_mode()),
        "mouse {label}"
    );
    assert_eq!(
        format!("{:?}", ours.mouse_protocol_encoding()),
        format!("{:?}", reference.mouse_protocol_encoding()),
        "encoding {label}"
    );
    reference.set_scrollback(usize::MAX);
    let history = reference.scrollback();
    assert_eq!(ours.history_len(), history, "history length {label}");
    for offset in 0..=history {
        reference.set_scrollback(offset);
        let window = ours.window(offset, rows, cols);
        for y in 0..rows {
            assert_eq!(
                window.row(y).ok_or(Error::InvalidRange)?.wrapped,
                reference.row_wrapped(y),
                "wrap {label} offset={offset} row={y}"
            );
            for x in 0..cols {
                let row = window.row(y).ok_or(Error::InvalidRange)?;
                let blank = Cell::default();
                let ours = row.cells.get(usize::from(x)).unwrap_or(&blank);
                let expected = reference.cell(y, x);
                if let Some(expected) = expected {
                    compare_cell(
                        ours,
                        expected,
                        &format!("{label} offset={offset} ({y},{x})"),
                    );
                } else {
                    assert!(!ours.has_contents(), "missing history cell {label}");
                }
            }
        }
    }
    reference.set_scrollback(0);
    Ok(())
}

#[test]
fn supported_operations_match_at_each_boundary_and_chunk_size() -> Result {
    let cases: &[(&str, &[&[u8]])] = &[
        (
            "text",
            &[b"hello", b" world\r\nnext", b"\tT\x08B", b"\x0bV\x0cF\rCR"],
        ),
        (
            "cursor",
            &[
                b"0123456789abcdefgh",
                b"\x1b[H",
                b"\x1b[2B\x1b[4C",
                b"\x1b[A\x1b[2D",
                b"\x1b[2E\x1b[F",
                b"\x1b[3G\x1b[2d",
                b"\x1b[999;999H",
            ],
        ),
        (
            "editing",
            &[
                b"abcdef\r",
                b"\x1b[2@",
                b"\x1b[2C\x1b[3P",
                b"\x1b[44m\x1b[2X",
                b"\x1b[1K",
                b"\x1b[2K",
                b"\x1b[1J",
                b"\x1b[2J",
            ],
        ),
        (
            "scrolling",
            &[
                b"one\r\ntwo\r\nthree\r\nfour\r\nfive",
                b"\x1b[H\x1b[S",
                b"\x1b[T",
                b"\x1b[L",
                b"\x1b[M",
                b"\x1bM",
            ],
        ),
        (
            "margins",
            &[
                b"a\r\nb\r\nc\r\nd",
                b"\x1b[2;3r",
                b"\x1b[?6h\x1b[2;1H",
                b"\r\n",
                b"\x1b[H\x1bM",
                b"\x1b[?6l\x1b[r",
            ],
        ),
        (
            "sgr",
            &[
                b"\x1b[1;3;4;7;31;44mA",
                b"\x1b[2;91;104mB",
                b"\x1b[22;23;24;27;39;49mC",
                b"\x1b[38:2:1:2:3;48:5:200mD",
                b"\x1b[38;5;255;48;2;5;6;7mE",
                b"\x1b[mF",
            ],
        ),
        (
            "modes",
            &[
                b"\x1b[?1h\x1b[?25l\x1b[?2004h",
                b"\x1b[?1l\x1b[?25h\x1b[?2004l",
                b"\x1b[?9h",
                b"\x1b[?1000h",
                b"\x1b[?1002h\x1b[?1000l",
                b"\x1b[?1003h\x1b[?1003l",
                b"\x1b[?1005h\x1b[?1006h\x1b[?1005l",
                b"\x1b[?1006l",
            ],
        ),
        (
            "alternate",
            &[
                b"one\r\ntwo\r\nthree\r\nfour\r\nfive",
                b"\x1b[31m\x1b[?1049hALT",
                b"\x1b[?1049lX",
                b"\x1b[?47h",
                b"\x1b[?47l",
            ],
        ),
        (
            "save-reply-reset",
            &[
                b"abc\x1b7",
                b"\x1b[2;2H\x1b[32mQ",
                b"\x1b8X",
                b"\x1b[5n\x1b[6n\x1b[c\x1b[?6n",
                b"\x1bc",
            ],
        ),
        (
            "unicode",
            &[
                "A界e\u{301}Z".as_bytes(),
                b"\r\n",
                "界界\u{301}".as_bytes(),
                b"\x1b[2;2Hx",
                b"\x1b[3G\x1b[X",
            ],
        ),
        (
            "strings-invalid",
            &[
                b"A\x1b]title\x07B",
                b"\x1bP1;2qpayload\x1b\\C",
                b"\x1b_ignore\x1b\\D",
                b"\x1b^ignore\x1b\\E",
                b"\x1bXignore\x1b\\F",
                b"\xc0\x80\xf0\x9f",
                b"\x1b[H!",
                b"\x1b[123\x18x",
                b"\x1b[12\x1ay",
            ],
        ),
    ];
    for &(name, operations) in cases {
        for chunk_size in [1, 2, 3, 7, usize::MAX] {
            let mut ours = Parser::new(4, 12, 3)?;
            let mut reference = vt100::Parser::new_with_callbacks(4, 12, 3, Replies::default());
            let mut replies = Vec::new();
            for (i, operation) in operations.iter().enumerate() {
                for chunk in operation.chunks(chunk_size) {
                    ours.process_with_replies(chunk, |r| replies.push(r.to_vec()))?;
                    reference.process(chunk);
                    compare(
                        ours.screen(),
                        reference.screen_mut(),
                        &format!("{name} op={i} chunk={chunk_size} bytes={chunk:?}"),
                    )?;
                    assert_eq!(replies, reference.callbacks().0, "replies {name} op={i}");
                }
            }
        }
    }
    Ok(())
}

#[test]
fn resize_and_history_match_for_retained_ascii_rows() -> Result {
    let mut ours = Parser::new(3, 8, 4)?;
    let mut reference = vt100::Parser::new(3, 8, 4);
    for operation in [b"abcdefghij\r\n".as_slice(), b"klmnopqrstuv\r\n", b"wxyz"] {
        ours.process(operation)?;
        reference.process(operation);
        compare(ours.screen(), reference.screen_mut(), "before resize")?;
    }
    for (rows, cols) in [(2, 4), (4, 12), (3, 8)] {
        ours.resize(rows, cols)?;
        reference.screen_mut().set_size(rows, cols);
        compare(
            ours.screen(),
            reference.screen_mut(),
            &format!("resize {rows}x{cols}"),
        )?;
    }
    Ok(())
}
