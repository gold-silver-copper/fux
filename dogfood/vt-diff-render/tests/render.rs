#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic, reason = "tests")]
use fux_vt::{Options, Parser, Size};
use vt_diff_render::{Renderer, first_difference};

fn parser(rows: u16, cols: u16) -> Parser {
    let size = Size::new(rows, cols).expect("size");
    Parser::with_options(size, 1 << 16, Options::default()).expect("parser")
}

/// Feeds `stream` in `chunk`-byte frames, renders each, and checks the outer
/// terminal matches; returns the repaint bytes and the renderer.
fn replay(stream: &[u8], rows: u16, cols: u16, chunk: usize) -> (usize, Renderer) {
    let (mut source, mut outer) = (parser(rows, cols), parser(rows, cols));
    let mut renderer = Renderer::new();
    let mut total = 0;
    for (frame, piece) in stream.chunks(chunk).enumerate() {
        let _ = source.process_all(piece, &mut ());
        let mut out = Vec::new();
        renderer.render(source.screen(), &mut out).expect("render");
        total += out.len();
        let _ = outer.process_all(&out, &mut ());
        if let Some(diff) = first_difference(source.screen(), outer.screen()) {
            let end = (frame + 1) * chunk;
            let tail = &stream[end.saturating_sub(300)..end.min(stream.len())];
            panic!("frame {frame} (chunk {chunk}): {diff}\nrepaint: {:?}\ninput tail: {:?}", String::from_utf8_lossy(&out), String::from_utf8_lossy(tail));
        }
    }
    (total, renderer)
}

/// A pseudo-random stream of what TUIs print: text in styles and colours,
/// wide and combining glyphs, cursor moves, erases, scrolls (up and down,
/// in regions), insertions, line renditions, the alternate screen.
fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    let mut next = |n: u64| {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 33) % n
    };
    let pieces: &[&str] = &[
        "hello", "wörld", "界", "日本語", "e\u{301}", "🙂", " ", "\r\n", "\n", "\t", "\x1b[K", "\x1b[1K",
        "\x1b[2J", "\x1b[J", "\x1b[3X", "\x1b[2@", "\x1b[2P", "\x1b[L", "\x1b[M", "\x1b[S", "\x1b[2T",
        "\x1bM", "\x1b[3;7r", "\x1b[r", "\x1b[1;31m", "\x1b[0m", "\x1b[4:3m", "\x1b[7m", "\x1b[44m",
        "\x1b[38;5;208m", "\x1b[48;2;10;20;30m", "\x1b[2m", "\x1b[22m", "\x1b#6", "\x1b#5", "\x1b[?25l",
        "\x1b[?25h", "\x1b[?1049h", "\x1b[?1049l", "\x1b[?5h", "\x1b[?5l", "\x1b[?7l", "\x1b[?7h",
    ];
    let mut out = Vec::new();
    while out.len() < len {
        match next(4) {
            0 => out.extend_from_slice(format!("\x1b[{};{}H", next(14) + 1, next(42) + 1).as_bytes()),
            _ => out.extend_from_slice(pieces[next(pieces.len() as u64) as usize].as_bytes()),
        }
    }
    out
}

#[test]
fn every_frame_of_noise_repaints_exactly() {
    for seed in 0..40 {
        let stream = noise(seed, 6000);
        for chunk in [1, 7, 64, 500] {
            replay(&stream, 12, 40, chunk);
        }
    }
}

#[test]
fn scrolling_output_is_sent_as_a_scroll() {
    let mut stream = Vec::new();
    for i in 0..400 {
        stream.extend_from_slice(format!("\x1b[32mline\x1b[0m {i:>4} of a long log that scrolls\r\n").as_bytes());
    }
    // One line a frame: each frame is a scroll and one new row.
    let line = stream.len() / 400;
    let (total, renderer) = replay(&stream, 24, 80, line);
    assert!(renderer.stats.scrolls >= 370, "{:?}", renderer.stats);
    // A whole repaint of a 24-row screen is over 1 KiB; a scroll and a
    // line are well under 100 bytes.
    assert!(total < 400 * 100, "{total} bytes");
}

#[test]
fn a_region_scrolling_either_way_is_sent_as_a_scroll() {
    // A pager: a status line below a 22-row region, scrolled forward 100
    // lines, then back 50 (RI at the region's top).
    let mut stream = b"\x1b[24;1H\x1b[7mstatus line\x1b[0m\x1b[1;23r".to_vec();
    let mut frames = vec![stream.len()];
    for i in 0..100 {
        stream.extend_from_slice(format!("\x1b[23;1H\n\x1b[34mforward\x1b[0m {i:>4} lorem ipsum dolor sit amet").as_bytes());
        frames.push(stream.len());
    }
    for i in 0..50 {
        stream.extend_from_slice(format!("\x1b[1;1H\x1bM\x1b[35mback\x1b[0m {i:>4} consectetur adipiscing").as_bytes());
        frames.push(stream.len());
    }
    let (mut source, mut outer) = (parser(24, 80), parser(24, 80));
    let mut renderer = Renderer::new();
    let mut total = 0;
    let mut at = 0;
    for end in frames {
        let _ = source.process_all(&stream[at..end], &mut ());
        at = end;
        let mut out = Vec::new();
        renderer.render(source.screen(), &mut out).expect("render");
        total += out.len();
        let _ = outer.process_all(&out, &mut ());
        assert_eq!(first_difference(source.screen(), outer.screen()), None);
    }
    assert!(renderer.stats.scrolls >= 145, "{:?}", renderer.stats);
    assert!(total < 150 * 100, "{total} bytes");
}
