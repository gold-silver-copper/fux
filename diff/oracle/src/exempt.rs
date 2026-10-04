//! The approved exemptions: sequences of features the user approved adding
//! to fux-vt after the commit the oracle compares against (phase 5 of the
//! work to beat Ghostty's core, `~/Desktop/code/fux-vt-beat-ghostty-prompt.md`,
//! "New behaviour"). The commit ignores them, or answers them otherwise, so
//! on them the two sides differ on purpose. Each is named here, with the
//! feature it belongs to; nothing else is exempt.
//!
//! The exemption is an input filter: every byte the oracle gives both sides
//! (each step's output, and each probe) goes through [`Filter`] first, which
//! takes out exactly these sequences, and the C0 controls inside one, which
//! execute wherever they are, stay. Both sides get the same filtered bytes,
//! and everything they show is compared as before: a sequence not named
//! here is passed on whole, byte for byte, so the oracle holds the rest of
//! fux-vt to the commit exactly as it did.
//!
//! - **The palette** (`Options::palette`): OSC 4, 5, 104, 105 and 110 to
//!   119, which set, query and reset the palette, the special colours and
//!   the dynamic colours; and OSC 10 to 19 when any of their parameters
//!   sets a colour. A query of OSC 10 to 19 alone (every parameter `?`)
//!   stays: with no colour set, fux-vt with the palette asks the host, as
//!   the commit does (`Event::ColorQuery`).
//! - **Reverse wraparound** (`CSI ? 45 h/l`, and xterm's extended form
//!   `CSI ? 1045 h/l`): BS and CUB then go back over the line's soft wraps.
//! - **The modes DECRQM answers, kept as xterm keeps them**: DECSCLM (4),
//!   DECSCNM (5), DECARM (8, answered permanently reset), DECNKM (66, the
//!   keypad mode, as ESC = and ESC > set it) and DECBKM (67).
//! - **XTSAVE and XTRESTORE** (`CSI ? Pm s`, `CSI ? Pm r`).
//! - **LNM** (`CSI 20 h/l`).
//! - **DECID** (`ESC Z`), answered as DA1.
//! - **DECALN** (`ESC # 8`).
//! - **Left and right margins**: DECLRMM (`CSI ? 69 h/l`) and its DECRQM;
//!   DECSLRM (`CSI Pl ; Pr s`), only while DECLRMM is set, which the
//!   filter follows as fux-vt would (DECSET and DECRST of 69, DECSTR and
//!   RIS resetting it, XTSAVE and XTRESTORE of it): while it is reset the
//!   sequence is SCOSC, on both sides, and passed on; and DECIC and DECDC
//!   (`CSI Pn ' }`, `CSI Pn ' ~`), the column edits the margins bound.
//! - **Protected glyphs and selective erase**: DECSCA (`CSI Ps " q`), SPA
//!   and EPA (`ESC V`, `ESC W`), DECSED (`CSI ? Ps J`) and DECSEL (`CSI ?
//!   Ps K`).
//!
//! For the modes: the mode is taken out of a DECSET or DECRST (`CSI ? Pm
//! h/l`, `CSI Pm h/l`) and the others in it are kept, and DECRQM of it
//! (`CSI ? Ps $ p`, `CSI Ps $ p`) is taken out whole. Only sequences in the
//! plain form fux-vt reads them in are touched (digits and `;`, a `?`
//! marker, then the intermediates the sequence has): any other form is
//! passed on, as both sides read it alike.
use crate::case::{Case, Step};

/// The DEC private modes of the approved features.
const DEC_MODES: &[u16] = &[4, 5, 8, 45, 66, 67, 69, 1045];
/// DECLRMM, the left and right margins' mode.
const LEFT_RIGHT: u16 = 69;
/// The ANSI modes of the approved features: LNM.
const ANSI_MODES: &[u16] = &[20];
/// The OSC commands of the palette: every one of them.
const PALETTE_OSC: &[&[u8]] = &[
    b"4", b"5", b"104", b"105", b"110", b"111", b"112", b"113", b"114", b"115", b"116", b"117",
    b"118", b"119",
];
/// The OSC commands that query the dynamic colours, or set them.
const DYNAMIC_OSC: &[&[u8]] = &[
    b"10", b"11", b"12", b"13", b"14", b"15", b"16", b"17", b"18", b"19",
];
/// The most parameters fux-vt keeps; a sequence with more is ignored, on
/// both sides.
const PARAMETERS: usize = 32;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    /// After ESC, and any intermediates.
    Escape,
    /// After ESC `[`.
    Csi,
    /// After ESC `]`.
    Osc,
    /// A DCS, SOS, PM or APC string: passed on as it comes.
    String,
}

/// Takes the approved sequences out of a stream of output, as it comes in
/// pieces: a sequence cut between two pieces is held until it is whole.
#[derive(Clone, Debug, Default)]
pub struct Filter {
    state: State,
    /// The sequence being read, from its ESC, while it is held.
    held: Vec<u8>,
    /// Whether DECLRMM is set in the stream, as fux-vt would have it: then
    /// `CSI Pl ; Pr s` is DECSLRM, and taken out.
    left_right: bool,
    /// DECLRMM as XTSAVE saved it, if it did.
    saved_left_right: Option<bool>,
}

impl Filter {
    /// Gives `bytes` to the filter, and what it lets through to `out`.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        for &b in bytes {
            self.byte(b, out);
        }
    }

    /// What the filter still holds: a sequence never ended, passed on as
    /// it is.
    pub fn finish(&mut self, out: &mut Vec<u8>) {
        out.append(&mut self.held);
        self.state = State::Ground;
    }

    fn start(&mut self, out: &mut Vec<u8>) {
        // A sequence cut short by ESC does nothing: passed on as it is.
        out.append(&mut self.held);
        self.held.push(ESC);
        self.state = State::Escape;
    }

    fn byte(&mut self, b: u8, out: &mut Vec<u8>) {
        match self.state {
            State::Ground => {
                if b == ESC {
                    self.start(out);
                } else {
                    out.push(b);
                }
            }
            State::String => {
                if b == ESC {
                    self.start(out);
                } else {
                    if matches!(b, CAN | SUB) {
                        self.state = State::Ground;
                    }
                    out.push(b);
                }
            }
            State::Escape | State::Csi | State::Osc if b == ESC => {
                if self.state == State::Osc {
                    // ESC ends the string (it begins ST) and starts a
                    // sequence of its own.
                    self.osc(out);
                }
                self.start(out);
            }
            State::Escape | State::Csi | State::Osc if matches!(b, CAN | SUB) => {
                // Cancelled: nothing carried out, so nothing to take out.
                out.append(&mut self.held);
                out.push(b);
                self.state = State::Ground;
            }
            State::Osc => {
                if b == BEL {
                    if self.osc(out) {
                        out.push(BEL);
                    }
                    self.state = State::Ground;
                } else {
                    self.held.push(b);
                }
            }
            State::Escape => {
                self.held.push(b);
                if !(0x30..=0x7e).contains(&b) {
                    return;
                }
                let intermediates: Vec<u8> = self
                    .held
                    .iter()
                    .skip(1)
                    .copied()
                    .filter(|c| (0x20..=0x2f).contains(c))
                    .collect();
                if intermediates.is_empty() {
                    match b {
                        b'[' => {
                            self.state = State::Csi;
                            return;
                        }
                        b']' => {
                            self.state = State::Osc;
                            return;
                        }
                        b'P' | b'X' | b'^' | b'_' => {
                            out.append(&mut self.held);
                            self.state = State::String;
                            return;
                        }
                        _ => {}
                    }
                }
                // RIS resets DECLRMM.
                if (intermediates.as_slice(), b) == ([].as_slice(), b'c') {
                    self.left_right = false;
                }
                // DECID (ESC Z), DECALN (ESC # 8), and SPA and EPA (ESC V,
                // ESC W).
                let exempt = matches!(
                    (intermediates.as_slice(), b),
                    ([], b'Z' | b'V' | b'W') | ([b'#'], b'8')
                );
                self.end(exempt, None, out);
            }
            State::Csi => {
                self.held.push(b);
                if (0x40..=0x7e).contains(&b) {
                    let held = std::mem::take(&mut self.held);
                    let rewritten = self.csi(&held);
                    self.held = held;
                    match rewritten {
                        Csi::Keep => self.end(false, None, out),
                        Csi::Drop => self.end(true, None, out),
                        Csi::Rewrite(sequence) => self.end(true, Some(sequence), out),
                    }
                }
            }
        }
    }

    /// Ends the sequence held: passed on, or, if `exempt`, taken out (its
    /// C0 controls kept) and `instead` put in its place.
    fn end(&mut self, exempt: bool, instead: Option<Vec<u8>>, out: &mut Vec<u8>) {
        if exempt {
            out.extend(self.held.iter().skip(1).filter(|c| **c < 0x20));
            out.extend(instead.unwrap_or_default());
            self.held.clear();
        } else {
            out.append(&mut self.held);
        }
        self.state = State::Ground;
    }

    /// Ends the OSC string held: passed on, or taken out if it is one of
    /// the palette's. Whether it was passed on.
    fn osc(&mut self, out: &mut Vec<u8>) -> bool {
        let payload = self.held.get(2..).unwrap_or_default();
        let (command, rest) = match payload.iter().position(|b| *b == b';') {
            Some(i) => (
                payload.get(..i).unwrap_or_default(),
                payload.get(i.saturating_add(1)..).unwrap_or_default(),
            ),
            None => (payload, &[][..]),
        };
        let exempt = PALETTE_OSC.contains(&command)
            || (DYNAMIC_OSC.contains(&command) && rest.split(|b| *b == b';').any(|p| p != b"?"));
        if exempt {
            self.held.clear();
        } else {
            out.append(&mut self.held);
        }
        self.state = State::Ground;
        !exempt
    }
}

/// What becomes of a CSI sequence.
enum Csi {
    Keep,
    Drop,
    Rewrite(Vec<u8>),
}

impl Filter {
    /// What becomes of `sequence`, a whole CSI sequence from its ESC; and
    /// DECLRMM as it leaves it.
    fn csi(&mut self, sequence: &[u8]) -> Csi {
        // What fux-vt reads of it: the C0 controls in it are carried out
        // where they are, and DEL and the bytes from 0x80 on are ignored
        // there, in the escape as in the CSI (Parser::byte); none of them
        // is part of the sequence. So `ESC BEL [ ? r` and `CSI 2 0xA9 0 l`
        // are XTRESTORE and LNM, as fux-vt reads them.
        let read: Vec<u8> = sequence
            .iter()
            .skip(1)
            .copied()
            .filter(|c| (0x20..=0x7e).contains(c))
            .collect();
        let Some((&last, body)) = read.get(1..).and_then(<[u8]>::split_last) else {
            return Csi::Keep;
        };
        let (private, rest) = match body.split_first() {
            Some((b'?', rest)) => (true, rest),
            _ => (false, body),
        };
        // The parameters, then the intermediates: a parameter byte after
        // an intermediate makes fux-vt ignore the sequence.
        let split = rest
            .iter()
            .position(|c| (0x20..=0x2f).contains(c))
            .unwrap_or(rest.len());
        let (digits, intermediates) = rest.split_at_checked(split).unwrap_or((rest, &[]));
        if !digits.iter().all(|c| c.is_ascii_digit() || *c == b';')
            || !intermediates.iter().all(|c| (0x20..=0x2f).contains(c))
        {
            return Csi::Keep;
        }
        let params: Vec<u16> = digits
            .split(|c| *c == b';')
            .map(|p| {
                p.iter().fold(0u16, |n, d| {
                    n.saturating_mul(10)
                        .saturating_add(u16::from(d.saturating_sub(b'0')))
                })
            })
            .collect();
        if params.len() > PARAMETERS {
            return Csi::Keep;
        }
        let first = params.first().copied().unwrap_or(0);
        let modes = if private { DEC_MODES } else { ANSI_MODES };
        match (private, intermediates, last) {
            // DECSTR resets DECLRMM, and is passed on.
            (false, b"!", b'p') => {
                self.left_right = false;
                Csi::Keep
            }
            // DECSCA.
            (false, b"\"", b'q') => Csi::Drop,
            // DECIC and DECDC.
            (false, b"'", b'}' | b'~') => Csi::Drop,
            // DECSED and DECSEL.
            (true, b"", b'J' | b'K') => Csi::Drop,
            // DECSLRM, while DECLRMM is set; SCOSC otherwise.
            (false, b"", b's') if self.left_right => Csi::Drop,
            // DECSET and DECRST, SM and RM: the approved modes out, the
            // rest kept.
            (_, b"", b'h' | b'l') => {
                if private && params.contains(&LEFT_RIGHT) {
                    self.left_right = last == b'h';
                }
                let kept: Vec<u16> = params
                    .iter()
                    .copied()
                    .filter(|n| !modes.contains(n))
                    .collect();
                if kept.len() == params.len() {
                    Csi::Keep
                } else if kept.is_empty() {
                    Csi::Drop
                } else {
                    let list: Vec<String> = kept.iter().map(u16::to_string).collect();
                    let marker = if private { "?" } else { "" };
                    let text = format!("\x1b[{marker}{}{}", list.join(";"), char::from(last));
                    Csi::Rewrite(text.into_bytes())
                }
            }
            // XTSAVE and XTRESTORE, DECLRMM among the modes they save and
            // restore.
            (true, b"", b's') => {
                if params.contains(&LEFT_RIGHT) {
                    self.saved_left_right = Some(self.left_right);
                }
                Csi::Drop
            }
            (true, b"", b'r') => {
                if params.contains(&LEFT_RIGHT) {
                    self.left_right = self.saved_left_right.unwrap_or(false);
                }
                Csi::Drop
            }
            // DECRQM of an approved mode.
            (_, b"$", b'p') if modes.contains(&first) => Csi::Drop,
            _ => Csi::Keep,
        }
    }
}

/// `bytes` with the approved sequences taken out, as a whole: a probe.
pub fn bytes(bytes: &[u8]) -> Vec<u8> {
    let mut filter = Filter::default();
    let mut out = Vec::with_capacity(bytes.len());
    filter.feed(bytes, &mut out);
    filter.finish(&mut out);
    out
}

/// `case` with the approved sequences taken out of its output, the output
/// of all its steps read as one stream: a sequence cut between two steps
/// is held, and given with the step that ends it.
pub fn case(case: &Case) -> Case {
    let mut filter = Filter::default();
    let mut steps: Vec<Step> = case
        .steps
        .iter()
        .map(|step| match step {
            Step::Output(b) => {
                let mut out = Vec::with_capacity(b.len());
                filter.feed(b, &mut out);
                Step::Output(out)
            }
            Step::Frame(b) => {
                let mut out = Vec::with_capacity(b.len());
                filter.feed(b, &mut out);
                Step::Frame(out)
            }
            Step::Resize(..) | Step::Copy(_) => step.clone(),
        })
        .collect();
    // What was held at the end goes with the last step of output.
    let mut rest = Vec::new();
    filter.finish(&mut rest);
    if !rest.is_empty() {
        let last = steps.iter_mut().rev().find_map(|step| match step {
            Step::Output(b) | Step::Frame(b) => Some(b),
            Step::Resize(..) | Step::Copy(_) => None,
        });
        if let Some(last) = last {
            last.extend(rest);
        }
    }
    Case {
        steps,
        ..case.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::bytes;

    #[test]
    fn exactly_the_approved_sequences_are_taken_out() {
        let cases: &[(&[u8], &[u8])] = &[
            // Bytes fux-vt ignores, or carries out, inside a sequence do
            // not hide it (the oracle's --cases 50000, seeds 1-3).
            (b"\x1b[2\xa90l", b""),
            (b"\x1b\x07[?r", b"\x07"),
            (b"\x1b[\x95?r", b""),
            (b"\x1b[?\x7f45h", b""),
            // Kept whole.
            (
                b"ab\x1b[?7;25h\x1b[4h\x1b[?6$p\x1b[4$p",
                b"ab\x1b[?7;25h\x1b[4h\x1b[?6$p\x1b[4$p",
            ),
            (
                b"\x1b[2;3r\x1b[s\x1b[u\x1b7\x1b8\x1bc",
                b"\x1b[2;3r\x1b[s\x1b[u\x1b7\x1b8\x1bc",
            ),
            (
                b"\x1b]10;?\x07\x1b]11;?;?\x1b\\",
                b"\x1b]10;?\x07\x1b]11;?;?\x1b\\",
            ),
            (
                b"\x1b]0;title\x07\x1b]8;;u\x1b\\",
                b"\x1b]0;title\x07\x1b]8;;u\x1b\\",
            ),
            (b"\x1b[?45:1h\x1b[?>45h", b"\x1b[?45:1h\x1b[?>45h"),
            (b"\x1bP$qm\x1b\\\x1b#3", b"\x1bP$qm\x1b\\\x1b#3"),
            // Taken out.
            (b"a\x1bZb\x1b#8c", b"abc"),
            (b"\x1b[?45h\x1b[?1045l\x1b[20h\x1b[?67;66;5;4;8h", b""),
            (b"\x1b[?7;45;25h", b"\x1b[?7;25h"),
            (b"\x1b[4;20l", b"\x1b[4l"),
            (b"\x1b[?7s\x1b[?7;25r", b""),
            (b"\x1b[?45$p\x1b[?1045$p\x1b[20$p\x1b[?8$p", b""),
            (
                b"\x1b]4;1;?\x07\x1b]104\x1b\\\x1b]10;#fff\x07\x1b]11;?;red\x1b\\",
                b"\x1b\\\x1b\\",
            ),
            (b"\x1b]112\x07\x1b]5;0;?\x07", b""),
            // A control inside a sequence taken out is still carried out.
            (b"\x1b[?4\n5h", b"\n"),
            (b"\x1b\rZ", b"\r"),
            // Left and right margins: DECSLRM only while DECLRMM is set,
            // as fux-vt sets and resets it; SCOSC otherwise.
            (b"\x1b[?69h\x1b[2;5s\x1b[s\x1b[?69$p", b""),
            (b"\x1b[2;5s\x1b[s", b"\x1b[2;5s\x1b[s"),
            (b"\x1b[?69h\x1b[?69l\x1b[2;5s", b"\x1b[2;5s"),
            (b"\x1b[?69h\x1b[!p\x1b[2;5s", b"\x1b[!p\x1b[2;5s"),
            (b"\x1b[?69hx\x1bc\x1b[s", b"x\x1bc\x1b[s"),
            (b"\x1b[?7;69h\x1b[s", b"\x1b[?7h"),
            (
                b"\x1b[?69h\x1b[?69s\x1b[?69l\x1b[?69r\x1b[2;5s\x1b[?69s\x1b[!p\x1b[?69r\x1b[s",
                b"\x1b[!p",
            ),
            (b"\x1b[?69r\x1b[s", b"\x1b[s"),
            (b"\x1b[?69h\x1b[?1:2s\x1b[1:2s", b"\x1b[?1:2s\x1b[1:2s"),
            (b"\x1b[3'}\x1b['~\x1b[2;3r", b"\x1b[2;3r"),
            // Protected glyphs and selective erase.
            (b"\x1b[1\"qa\x1b[\"q\x1bVb\x1bWc\x1b[?J\x1b[?2K", b"abc"),
            (
                b"\x1b[J\x1b[2K\x1b[3X\x1b[1 q\x1b[?1\"q\x1b[1$\"q\x1b[?2$J\x1b#V",
                b"\x1b[J\x1b[2K\x1b[3X\x1b[1 q\x1b[?1\"q\x1b[1$\"q\x1b[?2$J\x1b#V",
            ),
            // Cancelled, or cut short: nothing was carried out.
            (b"\x1b[?45\x18h", b"\x1b[?45\x18h"),
            (b"\x1b[?4\x1b[?45h", b"\x1b[?4"),
            (b"\x1b]4;1;?\x18", b"\x1b]4;1;?\x18"),
            (b"\x1b[?45", b"\x1b[?45"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                String::from_utf8_lossy(&bytes(input)),
                String::from_utf8_lossy(expected),
                "{:?}",
                String::from_utf8_lossy(input)
            );
        }
    }

    #[test]
    fn a_sequence_cut_between_steps_is_held_until_it_is_whole() {
        use crate::case::{Case, Step};
        use crate::model::Setup;
        let case = Case {
            rows: 1,
            cols: 1,
            history: 0,
            setup: Setup::default(),
            steps: vec![
                Step::Output(b"a\x1b[?4".to_vec()),
                Step::Resize(2, 2),
                Step::Output(b"5h\x1b[?7".to_vec()),
                Step::Output(b"h\x1b[".to_vec()),
            ],
        };
        let filtered = super::case(&case);
        assert_eq!(
            filtered.steps,
            vec![
                Step::Output(b"a".to_vec()),
                Step::Resize(2, 2),
                Step::Output(Vec::new()),
                Step::Output(b"\x1b[?7h\x1b[".to_vec()),
            ]
        );
    }
}
