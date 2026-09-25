//! Deterministic adversarial generator adapted from fux-fuzz at 9140af1.
//! It is intentionally independent of fux-fuzz's executable/package.

pub fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut x = *state;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

/// Byte streams emitted by terminal_edge's controlled children (no timing or
/// shell echo); the main screen has 30 numbered lines followed by MAIN-END.
pub fn terminal_edge() -> Vec<Vec<u8>> {
    let mut main = b"\x1b[2J\x1b[H".to_vec();
    for i in 1..=30 {
        main.extend_from_slice(format!("LINE-{i:02}\r\n").as_bytes());
    }
    main.extend_from_slice(b"MAIN-END");
    vec![
        main,
        b"\x1b[?1049h\x1b[2J\x1b[HALTERNATE".to_vec(),
        b"\x1b[?1049l".to_vec(),
        b"\x1b[?1h\x1b[2J\x1b[HAPP".to_vec(),
        b"\x1b[2J\x1b[HBEFORE\x9b22m\x85AFTER\x98junk\x9cEND".to_vec(),
    ]
}

pub fn operations(seed: u64, bytes: usize) -> Vec<Vec<u8>> {
    let mut state = seed;
    let words: [&[u8]; 6] = [
        b"alpha ",
        b"BETA ",
        "界".as_bytes(),
        "é".as_bytes(),
        b"\t",
        b"x",
    ];
    let mut length = 0;
    let mut out = Vec::new();
    while length < bytes {
        let r = splitmix(&mut state);
        let [_, a, b, ..] = r.to_le_bytes();
        let mut op = match r % 24 {
            0 => b"\x1b[2J".to_vec(),
            1 => format!(
                "\x1b[{};{}H",
                (a % 40).saturating_add(1),
                (b % 200).saturating_add(1)
            )
            .into_bytes(),
            2 => format!(
                "\x1b[{}m",
                [0, 1, 4, 7, 31, 42, 91]
                    .get(usize::from(a % 7))
                    .copied()
                    .unwrap_or(0)
            )
            .into_bytes(),
            3 => b"\x1b[K".to_vec(),
            4 => b"\x1b[".to_vec(),
            5 => b"\x1b".to_vec(),
            6 => vec![0x80 | (a % 32)],
            7 => vec![0xc0, 0x80],
            8 => vec![0x80 | (a & 0x3f)],
            9 => vec![0xf0, 0x9f],
            10 => vec![b'L'; 500],
            11 => format!(
                "\x1b[{};{}r",
                (a % 10).saturating_add(1),
                (b % 12).saturating_add(12)
            )
            .into_bytes(),
            12 => b"\x1b[r".to_vec(),
            13 => {
                if a.is_multiple_of(2) {
                    b"\x1b[?7l".to_vec()
                } else {
                    b"\x1b[?7h".to_vec()
                }
            }
            14 => {
                if a.is_multiple_of(2) {
                    b"\x1b[?6h".to_vec()
                } else {
                    b"\x1b[?6l".to_vec()
                }
            }
            15 => b"\r\n".to_vec(),
            16 => b"\x1b[1000000000000C".to_vec(),
            17 => b"\x1b]0;title\x07".to_vec(),
            18 => b"\x1b]52;c;bm9wZQ==\x07".to_vec(),
            19 => b"\x07\x08\x0b\x0c".to_vec(),
            20 => b"\x1b[?1049h".to_vec(),
            21 => b"\x1b[?1049l".to_vec(),
            22 => b"\x1b[38;2;1;2;3;48;5;200m".to_vec(),
            _ => words
                .get(usize::from(a % 6))
                .copied()
                .unwrap_or(b"x")
                .to_vec(),
        };
        // The loop runs while `length < bytes`; lengths are far below usize.
        op.truncate(bytes.saturating_sub(length));
        length = length.saturating_add(op.len());
        out.push(op);
    }
    out
}
