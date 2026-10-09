//! fux-vt, the baseline's and the current one, fed the same random output
//! in random pieces, with resizes between: after every piece, the same
//! replies and events, and the same screen -- every cell of the history and
//! the screen with its row's identity, version and wrap flag, the cursor,
//! every mode, the scroll region, the rows changed since the last look --
//! and the same errors.
//!
//! The output is what both mean the same by. Left out are SGR 5, 6, 8, 9,
//! 25, 28, 29, 58 and 59, CSI f, s and u, and characters that join the
//! grapheme cluster before them (joiners, variation selectors, emoji
//! modifiers, regional indicators, spacing marks): fux-vt 0.2.0 added them,
//! and they are tested against their own models in fux-vt's tests and
//! beside other terminals in fux-vt/compare.
use crate::rng::Rng;
use crate::{Outcome, bump, same_lines, times};

/// A sequence a program writes, well formed.
fn well_formed(r: &mut Rng) -> Vec<u8> {
    let pick = |r: &mut Rng, items: &[&str]| r.pick(items).copied().unwrap_or_default().to_owned();
    let number = |r: &mut Rng| {
        pick(
            r,
            &[
                "", "0", "1", "2", "3", "4", "5", "6", "7", "9", "12", "25", "99", "65535",
            ],
        )
    };
    let text = match r.below(20) {
        0..=2 => {
            let modes = [
                "1004", "1", "25", "2004", "1049", "47", "1047", "7", "6", "1000", "1002", "1003",
                "1006", "9999", "",
            ];
            let params: Vec<String> = (0..r.below(3).saturating_add(1))
                .map(|_| pick(r, &modes))
                .collect();
            format!("\x1b[?{}{}", params.join(";"), pick(r, &["h", "l"]))
        }
        3 => format!("\x1b[{} q", number(r)),
        4 => format!(
            "\x1b[{}m",
            (0..r.below(4))
                .map(|_| { pick(r, &["", "0", "1", "2", "3", "4", "7", "12", "99", "65535"],) })
                .collect::<Vec<_>>()
                .join(";")
        ),
        5 => format!(
            "\x1b[{}{}",
            number(r),
            pick(
                r,
                &[
                    "A", "B", "C", "D", "E", "F", "G", "d", "J", "K", "L", "M", "P", "@", "X", "S",
                    "T", "b"
                ]
            )
        ),
        6 => format!("\x1b[{};{}{}", number(r), number(r), pick(r, &["H", "r"])),
        7 => pick(
            r,
            &[
                "\x1b[6n",
                "\x1b[5n",
                "\x1b[c",
                "\x1b[>c",
                "\x1b[?6n",
                "\x1b[?25$p",
                "\x1b[4$p",
                "\x1b[0c",
            ],
        ),
        8 => pick(
            r,
            &[
                "\x1b]2;title\x07",
                "\x1b]0;both\x1b\\",
                "\x1b]1;icon\x07",
                "\x1b]52;c;aGVsbG8=\x07",
                "\x1b]52;c;?\x07",
                "\x07",
                "\x1bPq#0;2\x1b\\",
                "\x1b_apc\x1b\\",
                "\x1bXsos\x1b\\",
            ],
        ),
        9 => pick(
            r,
            &[
                "\x1b(B", "\x1b(0", "\x1b7", "\x1b8", "\x1bM", "\x1bD", "\x1bE", "\x1b=", "\x1b>",
                "\x1bc", "\x1bH",
            ],
        ),
        10 => pick(r, &["é", "界", "e\u{301}", "ｱ"]),
        11 => pick(
            r,
            &[
                "\r\n", "\n", "\r", "\x08", "\t", "\x0b", "\x0c", "\x0e", "\x0f",
            ],
        ),
        12..=14 => {
            let words = ["hello ", "world", "a", "界界", "long-line-", "x"];
            (0..r.below(8)).map(|_| pick(r, &words)).collect()
        }
        _ => pick(
            r,
            &[
                "abc",
                "0123456789",
                " ",
                "text\r\n",
                "wrap wrap wrap wrap wrap wrap wrap ",
            ],
        ),
    };
    text.into_bytes()
}

/// Bytes a hostile or broken program writes.
fn hostile(r: &mut Rng) -> Vec<u8> {
    // No 5 or 6, which could make SGR 5 or 6.
    let alphabet: &[u8] = b"\x1b\x1b\x1b[[[??00112447;; ;:hhllqqcmHJK\x07\n\r\x7f\x18\x1a]P\\\xc3\xa9\x9b(X_\x90\xe7\x95";
    (0..r.below(40))
        .map(|_| r.pick(alphabet).copied().unwrap_or(b'x'))
        .collect()
}

/// Output: well-formed sequences, now and then broken or hostile.
fn output(r: &mut Rng) -> Vec<u8> {
    let mut out: Vec<u8> = (0..r.below(12).saturating_add(1))
        .flat_map(|_| well_formed(r))
        .collect();
    if r.chance(15) {
        out.extend(hostile(r));
    }
    if r.chance(10) && !out.is_empty() {
        let at = r.below(out.len());
        out = out
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != at)
            .map(|(_, b)| *b)
            .collect();
    }
    out
}

macro_rules! stack {
    (
        $name:ident,
        $vt:ident,
        |$row:ident| $cells:expr,
        |$attrs:ident| $colors:expr,
        |$meta:ident| $identity:expr,
        |$events:ident, $extended:ident| $options:expr,
        |$screen:ident| $modes:expr
    ) => {
        mod $name {
            use std::fmt::Write;
            use $vt::{Attributes, Event, Mark, Options, Parser, Row, Sink};

            /// Attributes through the accessors both sides have, so a field
            /// one side's `Debug` shows and the other's lacks is no
            /// difference.
            fn attributes(a: Attributes) -> String {
                let (foreground, background) = {
                    let $attrs = a;
                    $colors
                };
                format!(
                    "fg {:?} bg {:?} bold {} dim {} italic {} underline {} inverse {}",
                    foreground,
                    background,
                    a.bold(),
                    a.dim(),
                    a.italic(),
                    a.underline(),
                    a.inverse()
                )
            }

            /// Each cell's contents, halves and attributes, whatever its layout.
            fn cells($row: Row<'_>) -> String {
                let mut out = String::new();
                for cell in $cells {
                    let _ = write!(
                        out,
                        "[{:?} wide {} continuation {} {}]",
                        cell.contents(),
                        cell.is_wide(),
                        cell.is_wide_continuation(),
                        attributes(cell.attributes())
                    );
                }
                out
            }

            /// A row's identity, version and soft wrap, through what each
            /// side has (accessors, in both since fux-vt 0.3).
            fn identity($meta: Row<'_>) -> ($vt::RowId, u64, bool) {
                $identity
            }

            #[derive(Default)]
            struct Heard(String);

            impl Sink for Heard {
                fn reply(&mut self, bytes: &[u8]) {
                    let _ = write!(self.0, "reply {bytes:?}; ");
                }
                fn event(&mut self, event: Event<'_>) {
                    let _ = write!(self.0, "event {event:?}; ");
                }
            }

            pub struct Terminal {
                parser: Parser,
                mark: Mark,
            }

            impl Terminal {
                pub fn new(rows: u16, cols: u16, history: usize, events: bool, extended: bool) -> Result<Terminal, String> {
                    let options = {
                        let ($events, $extended) = (events, extended);
                        $options
                    };
                    let parser = Parser::with_options(rows, cols, history, options).map_err(|e| format!("{e:?}"))?;
                    let mark = parser.screen().mark();
                    Ok(Terminal { parser, mark })
                }

                /// What processing the bytes gave back.
                pub fn process(&mut self, bytes: &[u8]) -> String {
                    let mut heard = Heard::default();
                    let result = self.parser.process_with(bytes, &mut heard);
                    format!("{result:?} {}", heard.0)
                }

                pub fn resize(&mut self, rows: u16, cols: u16) -> String {
                    format!("{:?}", self.parser.resize(rows, cols))
                }

                /// The screen, whole.
                pub fn screen(&mut self) -> String {
                    let s = self.parser.screen();
                    let mut out = String::new();
                    let (rows, cols) = s.size();
                    let modes: [bool; 8] = {
                        let $screen = s;
                        $modes
                    };
                    let _ = writeln!(
                        out,
                        "{rows}x{cols} cursor {:?} modes {modes:?} shape {} region {:?} mouse {:?} {:?} attributes {} history {} storage {}",
                        s.cursor_position(),
                        s.cursor_shape(),
                        s.scroll_region(),
                        s.mouse_protocol_mode(),
                        s.mouse_protocol_encoding(),
                        attributes(s.attributes()),
                        s.history_len(),
                        s.storage_cells(),
                    );
                    let retained = s.history_len().saturating_add(usize::from(rows));
                    for offset in (0..retained).rev() {
                        if let Some(row) = s.row_from_bottom(offset) {
                            let (id, version, wrapped) = identity(row);
                            let _ = writeln!(
                                out,
                                "{:?} v{} wrapped {} at {:?} {}",
                                id,
                                version,
                                wrapped,
                                s.offset_for_row(id),
                                cells(row)
                            );
                        }
                    }
                    let _ = writeln!(
                        out,
                        "changed {} refresh {} dirty {:?} live {:?}",
                        s.changed_since(self.mark),
                        s.full_refresh_since(self.mark),
                        s.dirty_rows_since(self.mark).map(|row| identity(row).0).collect::<Vec<_>>(),
                        s.dirty_live_rows_since(self.mark).map(|(y, row)| (y, identity(row).0)).collect::<Vec<_>>()
                    );
                    self.mark = s.mark();
                    out
                }
            }
        }
    };
}

// Both read cells through the row, which holds the text of long clusters,
// and colours through methods, as fux-vt packs them; both keep every
// opt-in option off, so they answer the same input the same way.
stack!(
    base,
    baseline_vt,
    |row| row.cells(),
    |a| (a.foreground(), a.background()),
    |row| (row.id(), row.version(), row.wrapped()),
    |events, extended| Options::new()
        .with_events(events)
        .with_extended_replies(extended),
    |s| [
        s.hide_cursor(),
        s.application_cursor(),
        s.application_keypad(),
        s.bracketed_paste(),
        s.focus_reporting(),
        s.alternate_screen(),
        s.autowrap(),
        s.origin_mode(),
    ]
);
stack!(
    cur,
    fux_vt,
    |row| row.cells(),
    |a| (a.foreground(), a.background()),
    |row| (row.id(), row.version(), row.wrapped()),
    |events, extended| Options::new()
        .set(fux_vt::Feature::Events, events)
        .set(fux_vt::Feature::ExtendedReplies, extended),
    |s| [
        !s.mode(fux_vt::Mode::ShowCursor),
        s.mode(fux_vt::Mode::ApplicationCursor),
        s.mode(fux_vt::Mode::ApplicationKeypad),
        s.mode(fux_vt::Mode::BracketedPaste),
        s.mode(fux_vt::Mode::FocusReporting),
        s.mode(fux_vt::Mode::AlternateScreen),
        s.mode(fux_vt::Mode::Autowrap),
        s.mode(fux_vt::Mode::Origin),
    ]
);

pub fn run(r: &mut Rng, scale: usize) -> Outcome {
    let (mut terminals, mut pieces, mut resizes) = (0u64, 0u64, 0u64);
    let size = |r: &mut Rng| {
        let n = match r.below(6) {
            0 => r.below(3).saturating_add(1),
            1 => r.below(300).saturating_add(1),
            _ => r.below(30).saturating_add(1),
        };
        u16::try_from(n).unwrap_or(1)
    };
    for case in 0..times(500, scale) {
        let (rows, cols) = (size(r), size(r));
        let history = r.pick(&[0usize, 1, 3, 20, 200]).copied().unwrap_or(3);
        let (events, extended) = (r.chance(50), r.chance(50));
        let mut a = base::Terminal::new(rows, cols, history, events, extended)?;
        let mut b = cur::Terminal::new(rows, cols, history, events, extended)?;
        let mut log: Vec<String> = Vec::new();
        for _ in 0..r.below(20).saturating_add(1) {
            let (ra, rb) = if r.chance(10) {
                let (rows, cols) = (size(r), size(r));
                log.push(format!("resize {rows}x{cols}"));
                bump(&mut resizes);
                (a.resize(rows, cols), b.resize(rows, cols))
            } else {
                let bytes = output(r);
                log.push(format!("{:?}", String::from_utf8_lossy(&bytes)));
                bump(&mut pieces);
                (a.process(&bytes), b.process(&bytes))
            };
            let context = || {
                format!(
                    "terminal {case}: {rows}x{cols}, history {history}, events {events}, extended replies {extended}, after:\n  {}",
                    log.join("\n  ")
                )
            };
            same_lines(&context(), &ra, &rb)?;
            same_lines(&context(), &a.screen(), &b.screen())?;
        }
        bump(&mut terminals);
    }
    Ok(format!(
        "{terminals} terminals, {pieces} pieces of output, {resizes} resizes"
    ))
}
