//! `--speed`: fux-vt beside its last release, parsing the same output, each
//! run timed by the CPU time of the thread (`fuxix::process::thread_cpu_time`),
//! so time spent waiting for a processor while other processes run is not
//! counted. The two run in turn, best of nine each, on a 50 by 200 screen
//! with 10,000 rows of history. Not a comparison of results, and not run
//! with the areas.
use std::time::Duration;

/// What a stream is made of: one line of output, repeated.
const STREAMS: &[(&str, &str)] = &[
    (
        "ascii",
        "The quick brown fox: printable ASCII 0123456789 abcdefghijklmnopqrstuvwxyz\r\n",
    ),
    (
        "sgr",
        "\x1b[1;38;5;196mred\x1b[0m \x1b[38;2;1;2;3;48;5;22mtrue\x1b[m plain text here \x1b[4munder\x1b[24m\r\n",
    ),
    (
        "cjk",
        "漢字かなカナ混じりの文章をたくさん表示します。テスト\r\n",
    ),
    (
        "emoji",
        "ok 👍 fire 🔥 heart ❤️ flag 🇯🇵 family 👨\u{200d}👩\u{200d}👧\u{200d}👦 e\u{301}\r\n",
    ),
    ("cursor", "\x1b[5;10Hxy\x1b[2K\x1b[3;1Habc\x1b[1;1H\x1b[J"),
];

/// Each run feeds the parser pieces of this size, as a PTY read would.
const PIECE: usize = 16 * 1024;

/// `bytes` in pieces of `PIECE`, the last one shorter.
fn pieces(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = bytes;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let (piece, tail) = rest.split_at_checked(PIECE).unwrap_or((rest, &[]));
        rest = tail;
        Some(piece)
    })
}

fn cpu() -> Result<Duration, String> {
    fuxix::process::thread_cpu_time().map_err(|e| format!("thread CPU time: {e}"))
}

macro_rules! timed {
    ($vt:ident, $bytes:expr) => {{
        let mut parser = $vt::Parser::new(50, 200, 10_000).map_err(|e| format!("{e:?}"))?;
        let start = cpu()?;
        for piece in pieces($bytes) {
            parser
                .process(std::hint::black_box(piece))
                .map_err(|e| format!("{e:?}"))?;
        }
        std::hint::black_box(parser.screen().cursor_position());
        cpu()?.saturating_sub(start)
    }};
}

/// The table: each stream's best time on each side, and their ratio.
pub fn run(scale: usize) -> Result<String, String> {
    let lines = crate::times(20_000, scale);
    let mut out = format!(
        "fux-vt parse time, thread CPU, best of 9, {lines} lines a stream\n{:<8} {:>12} {:>12} {:>7}",
        "stream", "baseline µs", "current µs", "ratio"
    );
    for (name, line) in STREAMS {
        let bytes: String = std::iter::repeat_n(*line, lines).collect();
        let bytes = bytes.as_bytes();
        let (mut baseline, mut current) = (Duration::MAX, Duration::MAX);
        for _ in 0..9 {
            baseline = baseline.min(timed!(baseline_vt, bytes));
            current = current.min(timed!(fux_vt, bytes));
        }
        let ratio = current.as_secs_f64() / baseline.as_secs_f64().max(f64::MIN_POSITIVE);
        out.push_str(&format!(
            "\n{name:<8} {:>12} {:>12} {ratio:>7.2}",
            baseline.as_micros(),
            current.as_micros()
        ));
    }
    Ok(out)
}
