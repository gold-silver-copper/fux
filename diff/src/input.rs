//! Input, in the baseline and the current fux: a client's raw bytes decode
//! to the same keys, pastes and focus changes, piece by piece, with the same
//! Escape deadline and the same inputs when it passes; every key encodes to
//! the same bytes for a pane, in either cursor mode, and every paste alike,
//! bracketed or not; a command line held for a new shell is due at the
//! same moment; and a pane's input queue takes, refuses and gives back the
//! same bytes under the same pushes and reads.
use crate::rng::Rng;
use crate::{Outcome, bump, same, times};

const PIECES: &[&[u8]] = &[
    b"a",
    b"Z",
    b"\r",
    b"\t",
    b"\x7f",
    b"\x00",
    b"\x01",
    b"\x1a",
    b"\x1c",
    b"\x1f",
    b"\x1b",
    b"\x1b\x1b",
    b"\x1bx",
    b"\x1b[A",
    b"\x1b[1;5C",
    b"\x1b[1;2",
    b"\x1bOA",
    b"\x1bOP",
    b"\x1bO",
    b"\x1b[3~",
    b"\x1b[5;3~",
    b"\x1b[15~",
    b"\x1b[24;8~",
    b"\x1b[27;5;97~",
    b"\x1b[97;5u",
    b"\x1b[13u",
    b"\x1b[Z",
    b"\x1b[I",
    b"\x1b[O",
    b"\x1b[M abc",
    b"\x1b[<0;1;1M",
    b"\x1b[?1;2c",
    b"\x1b[200~",
    b"\x1b[201~",
    b"\x1b[20",
    b"\x1b[2",
    b"\x1b[",
    "é".as_bytes(),
    "界".as_bytes(),
    b"\xe7\x95",
    b"\xc3",
    b"\xff",
    b"\x9b",
    b"paste text\n",
    b"\x1b[200~short\x1b[201~",
    b"\x1b[200~\x1b[201",
    b"\x1b[;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;A",
];

fn raw(r: &mut Rng) -> Vec<u8> {
    let mut out: Vec<u8> = (0..r.below(10).saturating_add(1))
        .flat_map(|_| r.pick(PIECES).copied().unwrap_or_default().to_vec())
        .collect();
    if r.chance(3) {
        out.extend_from_slice(b"\x1b[200~");
        let len = if r.chance(50) { 70_000 } else { 60_000 };
        out.extend(std::iter::repeat_n(b'p', len));
        out.extend_from_slice(b"\x1b[201~");
    }
    out
}

const KEYS: &[&str] = &[
    "a", "Z", "1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "?", "@", "[", "\\", "]", "^", "_",
    "-", " ", "é", "界", "Enter", "Tab", "BTab", "Escape", "Space", "BSpace", "Up", "Down", "Left",
    "Right", "Home", "End", "PageUp", "PageDown", "Insert", "Delete", "F1", "F4", "F5", "F9",
    "F12",
];

fn key(r: &mut Rng) -> String {
    let mut name = String::new();
    for prefix in ["C-", "M-", "S-"] {
        if r.chance(30) {
            name.push_str(prefix);
        }
    }
    name.push_str(r.pick(KEYS).copied().unwrap_or("a"));
    name
}

/// `$name` decodes and encodes with `$fux`; `$key_bytes` encodes a press
/// in a cursor mode, and `$shown` shows a decoded input, both as each
/// version takes them: the current fux's keys carry what a kitty-protocol
/// terminal said beyond the press, and its encoder the pane's key mode.
macro_rules! stack {
    ($name:ident, $fux:ident, $key_bytes:expr, $shown:expr) => {
        mod $name {
            use std::time::Instant;
            use $fux::decode::Decoder;
            use $fux::keys::KeyPress;
            use $fux::pane::InputQueue;

            #[derive(Default)]
            pub struct Decoding(Decoder);

            impl Decoding {
                /// The inputs from `bytes`, read at `now`, and whether and
                /// until when (after `start`) the decoder waits.
                pub fn bytes(&mut self, bytes: &[u8], now: Instant, start: Instant) -> String {
                    let mut inputs = Vec::new();
                    self.0.bytes(bytes, &mut inputs);
                    self.0.mark(now);
                    self.waits(inputs, start)
                }

                pub fn timeout(&mut self, start: Instant) -> String {
                    let mut inputs = Vec::new();
                    self.0.timeout(&mut inputs);
                    self.waits(inputs, start)
                }

                fn waits(&self, inputs: Vec<$fux::decode::Input>, start: Instant) -> String {
                    let shown: fn(&$fux::decode::Input) -> String = $shown;
                    let inputs: Vec<String> = inputs.iter().map(shown).collect();
                    format!(
                        "[{}] waiting {} until {:?}",
                        inputs.join(", "),
                        self.0.waiting(),
                        self.0
                            .deadline()
                            .map(|d| d.saturating_duration_since(start))
                    )
                }
            }

            /// A key's bytes in both cursor modes, if its name is a key.
            pub fn key(name: &str) -> String {
                let press = name.parse::<KeyPress>();
                let bytes = press.as_ref().ok().map(|press| {
                    let (mut normal, mut application) = (Vec::new(), Vec::new());
                    let key_bytes: fn(KeyPress, bool, &mut Vec<u8>) = $key_bytes;
                    key_bytes(*press, false, &mut normal);
                    key_bytes(*press, true, &mut application);
                    (normal, application)
                });
                format!(
                    "{press:?} {:?} {bytes:?}",
                    press.as_ref().map(ToString::to_string)
                )
            }

            pub fn paste(text: &str) -> (Vec<u8>, Vec<u8>) {
                let (mut plain, mut bracketed) = (Vec::new(), Vec::new());
                $fux::encode::paste(text, false, &mut plain);
                $fux::encode::paste(text, true, &mut bracketed);
                (plain, bracketed)
            }

            /// When a held command line is typed, after `start`.
            pub fn due(
                start: Instant,
                deadline: Instant,
                last_output: Option<Instant>,
            ) -> Option<std::time::Duration> {
                let typed = $fux::pane::Typed {
                    line: Vec::new(),
                    deadline,
                    last_output,
                };
                typed.due_at().checked_duration_since(start)
            }

            #[derive(Default)]
            pub struct Queue(InputQueue);

            impl Queue {
                pub fn push(&mut self, bytes: &[u8], with: bool) -> String {
                    let result = if with {
                        self.0.push_with(|out| out.extend_from_slice(bytes))
                    } else {
                        self.0.push(bytes)
                    };
                    format!("{:?} {}", result.map_err(|e| e.to_string()), self.shown())
                }

                pub fn advance(&mut self, n: usize) -> String {
                    self.0.advance(n);
                    self.shown()
                }

                pub fn drain(&mut self) -> String {
                    format!("{} {}", self.0.drain_all().len(), self.shown())
                }

                fn shown(&self) -> String {
                    let front = self.0.front().unwrap_or_default();
                    format!(
                        "empty {} front {} bytes {:?}",
                        self.0.is_empty(),
                        front.len(),
                        front.get(..16.min(front.len()))
                    )
                }
            }
        }
    };
}

stack!(
    base,
    baseline,
    baseline::encode::key_bytes,
    |input| format!("{input:?}")
);
stack!(
    cur,
    fux,
    |press, application, out| fux::encode::key_bytes(
        press.into(),
        fux::encode::KeyMode::legacy(application),
        out
    ),
    |input| {
        // A key shows as its press, as the baseline's: what a kitty-protocol
        // terminal said beyond the press, which no baseline key has, is
        // fux's own tests' to check (`decode::tests`).
        if let fux::decode::Input::Key(stroke) = input {
            return format!("Key({:?})", stroke.press);
        }
        format!("{input:?}")
    }
);

fn decoding(r: &mut Rng, cases: usize) -> Result<(u64, u64), String> {
    let (mut streams, mut timeouts) = (0u64, 0u64);
    for case in 0..cases {
        let start = std::time::Instant::now();
        let (mut a, mut b) = (base::Decoding::default(), cur::Decoding::default());
        let mut log = Vec::new();
        for tick in 0..r.below(8).saturating_add(1) {
            let now = start
                .checked_add(std::time::Duration::from_millis(
                    u64::try_from(tick).unwrap_or(0).saturating_mul(10),
                ))
                .unwrap_or(start);
            let (ra, rb) = if r.chance(25) {
                log.push("timeout".to_owned());
                bump(&mut timeouts);
                (a.timeout(start), b.timeout(start))
            } else {
                let bytes = raw(r);
                log.push(format!(
                    "{:?}",
                    String::from_utf8_lossy(bytes.get(..bytes.len().min(80)).unwrap_or_default())
                ));
                (a.bytes(&bytes, now, start), b.bytes(&bytes, now, start))
            };
            same(&format!("input {case}, after {log:?}"), ra, rb)?;
        }
        bump(&mut streams);
    }
    Ok((streams, timeouts))
}

pub fn run(r: &mut Rng, scale: usize) -> Outcome {
    let (streams, timeouts) = decoding(r, times(20_000, scale))?;
    let mut keys = 0u64;
    for _ in 0..times(20_000, scale) {
        let name = key(r);
        same(
            &format!("the key {name:?}"),
            base::key(&name),
            cur::key(&name),
        )?;
        bump(&mut keys);
    }
    let mut pastes = 0u64;
    let parts = [
        "text",
        "\x1b[201~",
        "\x1b[200~",
        "\n",
        "界",
        "\x1b",
        "\x1b[201",
        "201~",
        "",
    ];
    for _ in 0..times(5_000, scale) {
        let text: String = (0..r.below(6))
            .map(|_| r.pick(&parts).copied().unwrap_or_default())
            .collect();
        same(
            &format!("the paste {text:?}"),
            base::paste(&text),
            cur::paste(&text),
        )?;
        bump(&mut pastes);
    }
    let mut held = 0u64;
    let start = std::time::Instant::now();
    let later = |r: &mut Rng| {
        let wait = match r.below(4) {
            0 => std::time::Duration::from_secs(u64::MAX / 2),
            1 => std::time::Duration::ZERO,
            _ => std::time::Duration::from_millis(u64::try_from(r.below(200)).unwrap_or(0)),
        };
        start.checked_add(wait)
    };
    for case in 0..times(20_000, scale) {
        let deadline = later(r).unwrap_or(start);
        let last_output = if r.chance(33) { None } else { later(r) };
        same(
            &format!("held line {case}: deadline {deadline:?} last output {last_output:?}"),
            base::due(start, deadline, last_output),
            cur::due(start, deadline, last_output),
        )?;
        bump(&mut held);
    }
    let mut ops = 0u64;
    for case in 0..times(500, scale) {
        let (mut a, mut b) = (base::Queue::default(), cur::Queue::default());
        let mut log = Vec::new();
        for _ in 0..r.below(60) {
            let (ra, rb) = match r.below(10) {
                0..=5 => {
                    let len = match r.below(4) {
                        0 => 65_548,
                        1 => r.below(65_548),
                        _ => r.below(100),
                    };
                    let bytes: Vec<u8> = std::iter::repeat_n(b'q', len).collect();
                    let with = r.chance(50);
                    log.push(format!("push {len} with {with}"));
                    (a.push(&bytes, with), b.push(&bytes, with))
                }
                6..=8 => {
                    let n = r.below(200_000);
                    log.push(format!("advance {n}"));
                    (a.advance(n), b.advance(n))
                }
                _ => {
                    log.push("drain".to_owned());
                    (a.drain(), b.drain())
                }
            };
            same(&format!("queue {case}, after {log:?}"), ra, rb)?;
            bump(&mut ops);
        }
    }
    Ok(format!(
        "{streams} input streams ({timeouts} Escape timeouts), {keys} keys, {pastes} pastes, {held} held command lines, {ops} queue operations"
    ))
}
