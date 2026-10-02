//! Families of output a program writes, each with what it exercises and
//! whether fux-vt and the panel are expected to agree on it. A random run
//! draws only from families expected to agree, so any difference it finds
//! is a regression; `--family` and `--all` add the others. A family where
//! the panel splits and fux-vt follows a recorded choice is checked by
//! `verdicts`, beside xterm alone.
use crate::rng::Rng;

/// Whether fux-vt and the panel are expected to agree on a family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Agree,
    /// A recorded verdict: the panel splits, and fux-vt does what the
    /// references say, or what xterm does where xterm departs from them
    /// (fux-vt's README lists those departures) or they are silent. Says
    /// which, and where the engines stand. Left out of a random run, and
    /// checked beside xterm alone by `verdicts`.
    Decided(&'static str),
    /// They differ, and why: a fux-vt defect to fix, or a documented choice.
    Differs(&'static str),
}

pub struct Family {
    pub name: &'static str,
    pub about: &'static str,
    pub status: Status,
    /// Whether the family needs a parser set up as ratty sets it up
    /// (reflow, kitty keyboard, an identity): without it, fux-vt differs by
    /// design.
    pub ratty_only: bool,
    pub generate: fn(&mut Rng) -> Vec<u8>,
}

fn pick(r: &mut Rng, items: &[&str]) -> String {
    r.pick(items).copied().unwrap_or_default().to_owned()
}

/// A parameter as programs write them: mostly small, sometimes absent,
/// zero or past the screen.
fn number(r: &mut Rng) -> String {
    pick(
        r,
        &["", "0", "1", "1", "2", "2", "3", "4", "5", "7", "10", "99"],
    )
}

fn csi(r: &mut Rng, finals: &[&str]) -> String {
    format!("\x1b[{}{}", number(r), pick(r, finals))
}

fn text(r: &mut Rng) -> Vec<u8> {
    let words = [
        "hello ",
        "world",
        "a",
        "x",
        " ",
        "  ",
        "0123456789",
        "long-line-",
        "abc",
        "~!@#",
    ];
    let count = r.below(6).saturating_add(1);
    (0..count)
        .map(|_| pick(r, &words))
        .collect::<String>()
        .into_bytes()
}

fn wide(r: &mut Rng) -> Vec<u8> {
    pick(
        r,
        &["界", "界界", "あいう", "a界", "ｱｲ", "全角テキスト", "x界y"],
    )
    .into_bytes()
}

fn clusters(r: &mut Rng) -> Vec<u8> {
    pick(
        r,
        &[
            "e\u{301}",
            "a\u{300}\u{301}",
            "👍",
            "👍🏽",
            "👨\u{200d}👩\u{200d}👧",
            "🇺🇸",
            "🇺",
            "❤\u{fe0f}",
            "1\u{fe0f}\u{20e3}",
            "\u{1100}\u{1161}\u{11a8}",
            "क\u{94d}ष",
            "\u{301}",
        ],
    )
    .into_bytes()
}

fn controls(r: &mut Rng) -> Vec<u8> {
    pick(
        r,
        &["\r", "\n", "\r\n", "\x08", "\t", "\x0b", "\x0c", "\x07"],
    )
    .into_bytes()
}

fn cursor(r: &mut Rng) -> Vec<u8> {
    if r.chance(25) {
        format!("\x1b[{};{}{}", number(r), number(r), pick(r, &["H", "f"])).into_bytes()
    } else {
        csi(r, &["A", "B", "C", "D", "E", "F", "G", "d", "`", "a", "e"]).into_bytes()
    }
}

fn erase(r: &mut Rng) -> Vec<u8> {
    match r.below(4) {
        0 => format!("\x1b[{}J", pick(r, &["", "0", "1", "2"])),
        1 => format!("\x1b[{}K", pick(r, &["", "0", "1", "2"])),
        2 => csi(r, &["X"]),
        _ => format!(
            "\x1b[{}J\x1b[{}K",
            pick(r, &["", "1"]),
            pick(r, &["", "1", "2"])
        ),
    }
    .into_bytes()
}

fn edit(r: &mut Rng) -> Vec<u8> {
    csi(r, &["@", "P", "L", "M"]).into_bytes()
}

fn scroll(r: &mut Rng) -> Vec<u8> {
    match r.below(4) {
        0 => csi(r, &["S", "T"]),
        1 => format!("\x1b[{};{}r", number(r), number(r)),
        2 => pick(r, &["\x1bM", "\x1bM\x1bM"]),
        _ => format!("\x1b[{};{}r\x1b[{}H", number(r), number(r), number(r)),
    }
    .into_bytes()
}

fn index(r: &mut Rng) -> Vec<u8> {
    pick(r, &["\x1bD", "\x1bE", "\x1bD\x1bD"]).into_bytes()
}

fn origin(r: &mut Rng) -> Vec<u8> {
    format!(
        "\x1b[{};{}r\x1b[?6{}{}",
        number(r),
        number(r),
        pick(r, &["h", "l"]),
        String::from_utf8_lossy(&cursor(r))
    )
    .into_bytes()
}

fn autowrap(r: &mut Rng) -> Vec<u8> {
    pick(r, &["\x1b[?7l", "\x1b[?7h"]).into_bytes()
}

fn pending_wrap(r: &mut Rng) -> Vec<u8> {
    // Fill to the last column of a line, then act while the wrap is pending.
    let then = pick(
        r,
        &[
            "\x08",
            "\n",
            "\r",
            "\t",
            "\x1b[D",
            "\x1b[A",
            "\x1b[B",
            "\x1b[C",
            "\x1b[K",
            "\x1b[1K",
            "\x1b[X",
            "\x1b[@",
            "\x1b[P",
            "\x1bM",
            "\x1b[L",
            "\x1b[M",
            "\x1b[G",
            "\x1b[d",
            "\x1b7",
            "\x1b[s",
            "\x1b[6n",
            "\x1b[m",
            "\x1b[?25l",
        ],
    );
    format!("\x1b[999C{}{then}", pick(r, &["x", "界", "ab"])).into_bytes()
}

fn sgr(r: &mut Rng) -> Vec<u8> {
    // No empty parameter in a list: Ghostty ignores a trailing one, where
    // xterm reads it as 0 (`replay '\e[2;mX'`). Alone it is `CSI m`.
    if r.chance(10) {
        return b"\x1b[m".to_vec();
    }
    let codes = [
        "0",
        "1",
        "2",
        "3",
        "4",
        "5",
        "6",
        "7",
        "8",
        "9",
        "21",
        "22",
        "23",
        "24",
        "25",
        "27",
        "28",
        "29",
        "30",
        "31",
        "37",
        "39",
        "40",
        "41",
        "47",
        "49",
        "90",
        "97",
        "100",
        "107",
        "38;5;0",
        "38;5;123",
        "38;5;255",
        "48;5;17",
        "38;2;1;2;3",
        "48;2;255;0;128",
        "58;5;9",
        "58;2;4;5;6",
        "59",
    ];
    let count = r.below(3).saturating_add(1);
    let params: Vec<String> = (0..count).map(|_| pick(r, &codes)).collect();
    format!("\x1b[{}m", params.join(";")).into_bytes()
}

fn sgr_colon(r: &mut Rng) -> Vec<u8> {
    pick(
        r,
        &[
            "\x1b[38:2::255:0:0m",
            "\x1b[48:2::0:255:0m",
            "\x1b[38:2:1:2:3m",
            "\x1b[38:5:200m",
            "\x1b[48:5:17m",
            "\x1b[58:2::9:8:7m",
            "\x1b[58:5:3m",
            "\x1b[4:0m",
            "\x1b[4:1m",
            "\x1b[4:3m",
            "\x1b[1;38:2::10:20:30;4m",
        ],
    )
    .into_bytes()
}

fn sgr_invalid(r: &mut Rng) -> Vec<u8> {
    pick(
        r,
        &[
            "\x1b[38;5;300;1m",
            "\x1b[38;2;1;2m",
            "\x1b[38;9;1m",
            "\x1b[48;5m",
            "\x1b[38;2;256;0;0;3m",
        ],
    )
    .into_bytes()
}

fn save(r: &mut Rng) -> Vec<u8> {
    pick(r, &["\x1b7", "\x1b8", "\x1b[s", "\x1b[u"]).into_bytes()
}

fn alternate(r: &mut Rng) -> Vec<u8> {
    let modes = ["1049", "1049", "47", "1047", "1048"];
    format!("\x1b[?{}{}", pick(r, &modes), pick(r, &["h", "l"])).into_bytes()
}

fn modes(r: &mut Rng) -> Vec<u8> {
    match r.below(3) {
        0 => pick(r, &["\x1b=", "\x1b>"]),
        _ => format!(
            "\x1b[?{}{}",
            pick(r, &["1", "25", "2004", "1004", "1;25"]),
            pick(r, &["h", "l"])
        ),
    }
    .into_bytes()
}

fn tabs(r: &mut Rng) -> Vec<u8> {
    match r.below(4) {
        0 => pick(r, &["\x1bH", "\x1b[g", "\x1b[0g", "\x1b[3g"]),
        1 => csi(r, &["I", "Z"]),
        _ => "\t".to_owned(),
    }
    .into_bytes()
}

fn charset(r: &mut Rng) -> Vec<u8> {
    pick(
        r,
        &[
            "\x1b(0", "\x1b(B", "\x1b)0", "\x1b)B", "\x0e", "\x0f", "lqqk", "x  x", "mqqj", "`afgn",
        ],
    )
    .into_bytes()
}

fn repeat(r: &mut Rng) -> Vec<u8> {
    format!("{}{}", pick(r, &["-", "=", "界", ""]), csi(r, &["b"])).into_bytes()
}

fn titles(r: &mut Rng) -> Vec<u8> {
    pick(
        r,
        &[
            "\x1b]0;both\x07",
            "\x1b]2;title\x07",
            "\x1b]2;st\x1b\\",
            "\x1b]1;icon\x07",
            "\x1b]2;\x07",
            "\x1b]2;tïtle 界\x07",
        ],
    )
    .into_bytes()
}

fn strings(r: &mut Rng) -> Vec<u8> {
    string(r, &["abc", "q#0;2", "", "1$r", "é"])
}

/// Strings whose UTF-8 payload holds the byte 0x9c, which is ST in 8-bit
/// controls.
fn strings_c1(r: &mut Rng) -> Vec<u8> {
    string(r, &["✜", "М", "a✜b"])
}

fn string(r: &mut Rng, payloads: &[&str]) -> Vec<u8> {
    let payload = pick(r, payloads);
    let (open, close) = match r.below(4) {
        0 => ("\x1bP", "\x1b\\"),
        1 => ("\x1b_", "\x1b\\"),
        2 => ("\x1b^", "\x1b\\"),
        _ => ("\x1bX", "\x1b\\"),
    };
    format!("{open}{payload}{close}").into_bytes()
}

fn reset(r: &mut Rng) -> Vec<u8> {
    pick(r, &["\x1bc", "\x1b[!p"]).into_bytes()
}

fn kitty(r: &mut Rng) -> Vec<u8> {
    match r.below(3) {
        0 => format!("\x1b[>{}u", pick(r, &["1", "3", "31"])),
        1 => format!("\x1b[<{}u", pick(r, &["", "1", "2"])),
        // No empty mode: wezterm drops a CSI whose last parameter is
        // empty (`replay --engines wezterm '\e[=1;u'`), where the kitty
        // protocol reads it as 1.
        _ => format!(
            "\x1b[={};{}u",
            pick(r, &["1", "4", "8"]),
            pick(r, &["1", "2", "3"])
        ),
    }
    .into_bytes()
}

fn reports(r: &mut Rng) -> Vec<u8> {
    pick(r, &["\x1b[6n", "\x1b[5n"]).into_bytes()
}

fn insert_mode(r: &mut Rng) -> Vec<u8> {
    // Back over text already written, so there is something to insert into.
    pick(r, &["\x1b[4h\r", "\x1b[4h\x1b[3D", "\x1b[4h", "\x1b[4l"]).into_bytes()
}

fn invalid_utf8(r: &mut Rng) -> Vec<u8> {
    match r.below(5) {
        0 => vec![0xff],
        1 => vec![0xc3],
        2 => vec![0x80, b'a'],
        3 => vec![0xe7, 0x95, b'b'],
        _ => "\u{fffd}".as_bytes().to_vec(),
    }
}

fn hostile(r: &mut Rng) -> Vec<u8> {
    let alphabet: &[u8] =
        b"\x1b\x1b\x1b[[[??00112447;; ;:hhllqqcmHJKr\x07\n\r\x08\x7f\x18\x1a]P\\\xc3\xa9\x9b(X_\x90\xe7\x95";
    (0..r.below(24))
        .map(|_| r.pick(alphabet).copied().unwrap_or(b'x'))
        .collect()
}

/// Not output: a resize between pieces of output, with reflow.
fn resize(_: &mut Rng) -> Vec<u8> {
    Vec::new()
}

pub const RESIZE: &str = "resize";

pub const FAMILIES: &[Family] = &[
    Family {
        name: "text",
        about: "printable ASCII, spaces and lines long enough to wrap",
        status: Status::Agree,
        ratty_only: false,
        generate: text,
    },
    Family {
        name: "wide",
        about: "wide (CJK) and halfwidth characters, at and across the right edge",
        status: Status::Differs(
            "on a one-column screen fux-vt drops a wide glyph without moving the cursor, a choice its README documents and no reference settles; every other engine differs: xterm, Ghostty, libvterm and avt wrap and leave the cell blank, alacritty and wezterm draw the glyph (`replay --engines all --size 2x1 ' \\u{3046}'`)",
        ),
        ratty_only: false,
        generate: wide,
    },
    Family {
        name: "clusters",
        about: "grapheme clusters: combining marks, emoji modifiers and ZWJ sequences, flags, VS16, jamo, conjuncts",
        status: Status::Differs(
            "edge-case widths differ: a lone regional indicator or emoji modifier, a cluster widened in the last column (fux-vt keeps it narrow there, Ghostty wraps it), a zero-width jamo after another script; to look into",
        ),
        ratty_only: false,
        generate: clusters,
    },
    Family {
        name: "controls",
        about: "CR, LF, VT, FF, BS, HT and BEL",
        status: Status::Decided(
            "LF, VT and FF end a pending wrap, as DEC STD 070 (Appendix D.6.1) says and xterm does; HT keeps it, as xterm and every engine but avt do, though the appendix lists HT (a departure: fux-vt/README.md). The panel splits on a line feed that scrolls while a wrap is pending: xterm, Ghostty, wezterm and xterm.js end the wrap, alacritty, libvterm and avt keep it and outvote fux-vt (`replay --engines all --size 2x3 'abcdef\\nX'`)",
        ),
        ratty_only: false,
        generate: controls,
    },
    Family {
        name: "cursor",
        about: "CUU CUD CUF CUB CNL CPL CHA VPA HPA HPR VPR, CUP and HVP",
        status: Status::Differs(
            "fux-vt does what xterm does (0 of 1200 cases differ with xterm alone voting), and the panel splits only where a soft-wrapped row is written over again once the cursor moves back into it, as in `save`: xterm, libvterm, avt and xterm.js keep the row soft-wrapped, as fux-vt does; Ghostty, alacritty, wezterm and tmux end the wrap, and outvote fux-vt (`replay --engines all --size 4x5 '\\e[0e long-line-\\e[;99Habcabc~!@#a'`)",
        ),
        ratty_only: false,
        generate: cursor,
    },
    Family {
        name: "erase",
        about: "ED, EL and ECH",
        status: Status::Decided(
            "ED, EL and ECH end a pending wrap, as DEC STD 070 (Appendix D.6.1) says and xterm and Ghostty do; alacritty, libvterm, avt, wezterm and tmux keep it, as xterm.js does but on ECH, and outvote fux-vt (`replay --engines all --size 2x4 'abcd\\e[KX'`)",
        ),
        ratty_only: false,
        generate: erase,
    },
    Family {
        name: "edit",
        about: "ICH, DCH, IL and DL",
        status: Status::Differs(
            "the panel splits: IL and DL leave the cursor in the first column (DEC STD 070, IL and DL, note 2) in xterm, Ghostty and xterm.js, as in fux-vt, where alacritty, libvterm, avt, wezterm and tmux keep its column (`replay --engines all --size 3x5 'ab\\e[LX'`); and ICH, DCH, IL and DL end a pending wrap in xterm, Ghostty and xterm.js, as DEC STD 070 (Appendix D.6.1) says and fux-vt does, where alacritty, libvterm, avt and tmux keep it (`replay --engines all --size 2x5 'abcde\\e[L'`)",
        ),
        ratty_only: false,
        generate: edit,
    },
    Family {
        name: "scroll",
        about: "SU, SD, DECSTBM and RI",
        status: Status::Differs(
            "the panel splits where fux-vt does what DEC STD 070 and xterm do: an invalid DECSTBM (5-25, note 2) is ignored in xterm, Ghostty, wezterm, xterm.js and tmux, while alacritty, libvterm and avt home the cursor and outvote fux-vt (`replay --engines all --size 1x2 'x\\e[4;5r'`); RI ends a pending wrap (Appendix D.6.1) in xterm, wezterm and xterm.js, Ghostty, alacritty, libvterm and avt keep it (`replay --engines all --size 1x7 'abcdefg\\eM'`). Two differences from xterm remain, outside the audit's items: `CSI 0 T` scrolls a line, where xterm (reading it as mouse tracking), Ghostty, alacritty and libvterm do nothing, and fux-vt's parameters do not tell 0 from empty (`replay --engines all --size 2x1 '\\e[2;4r' '-\\e[0T'`); SD in a region clears the soft-wrap flag of the row it moves to the bottom margin, which xterm, alacritty and wezterm keep (`replay --engines all --size 6x1 '~!@#\\e[3;5r\\e[10H\\e[2T'`)",
        ),
        ratty_only: false,
        generate: scroll,
    },
    Family {
        name: "index",
        about: "IND (ESC D) and NEL (ESC E)",
        status: Status::Differs(
            "the panel splits as it does on LF in `controls`: IND that scrolls while a wrap is pending ends the wrap in xterm, Ghostty, wezterm and xterm.js, as DEC STD 070 (Appendix D.6.1) says and fux-vt does; alacritty, libvterm and avt keep it, and outvote fux-vt (`replay --engines all --size 1x3 'abc\\eD'`). With xterm alone voting, 0 of 1000 cases differ",
        ),
        ratty_only: false,
        generate: index,
    },
    Family {
        name: "origin",
        about: "DECOM with a scroll region, then cursor movement",
        status: Status::Differs(
            "fux-vt addresses lines in origin mode as DEC STD 070 (DECOM) and xterm do, and the panel splits on an invalid DECSTBM (see scroll) and on one-column screens, where Ghostty and alacritty wrap after HPA (`replay --engines all --size 11x1 '\\e[10;99r\\e[?6h\\e[2`o'`). Ghostty, alacritty and xterm.js count VPR in origin mode from the top margin twice, outvoted by xterm, libvterm, avt and wezterm (`replay --engines all --size 6x5 '\\e[2;6r\\e[?6h\\e[3eX'`). Against xterm alone, what differs is history: xterm, Ghostty and alacritty keep the lines that scroll off a region whose top is the screen's, and fux-vt keeps only those of whole-screen scrolls (documented)",
        ),
        ratty_only: false,
        generate: origin,
    },
    Family {
        name: "autowrap",
        about: "DECAWM on and off",
        status: Status::Decided(
            "with DECAWM off a glyph in the last column leaves a wrap pending, which DEC STD 070 (Appendix D.6.1) does not, and resetting DECAWM keeps one, though the appendix lists it among what clears it: fux-vt follows xterm in both (departures: fux-vt/README.md). xterm, Ghostty, alacritty and xterm.js leave the wrap pending with DECAWM off; libvterm, avt, wezterm and tmux do not, and outvote fux-vt in the default panel (`replay --engines all --size 2x2 '\\e[?7lca\\e[?7hX'`)",
        ),
        ratty_only: false,
        generate: autowrap,
    },
    Family {
        name: "pending-wrap",
        about: "a glyph in the last column, then a control, movement or edit while the wrap is pending",
        status: Status::Differs(
            "the panel splits: EL, ECH, ICH, DCH, IL, DL and a line feed that scrolls end a pending wrap in xterm and Ghostty, as DEC STD 070 (Appendix D.6.1) says and fux-vt does; alacritty, libvterm and avt keep it, and outvote fux-vt (`replay --engines all --size 1x5 'abcde\\e[KX'`). Also the one-column split of `wide`",
        ),
        ratty_only: false,
        generate: pending_wrap,
    },
    Family {
        name: "sgr",
        about: "SGR in semicolon form: every attribute, its reset, 16, 256 and RGB colours, underline colour",
        status: Status::Decided(
            "SGR 21 is doubly underlined (ECMA-48, 8.3.117): an underline beside bold, as xterm, Ghostty, libvterm, xterm.js and tmux read it, where alacritty and avt take it as bold off. Bold and faint can both be on, as in xterm, though ECMA-48 makes them one attribute (a departure: fux-vt/README.md); avt and wezterm let faint replace bold. Together they outvote fux-vt (`replay --engines all --size 1x3 '\\e[1m\\e[21;2mX'`). xterm has no SGR 58, and abstains from a case that uses it",
        ),
        ratty_only: false,
        generate: sgr,
    },
    Family {
        name: "sgr-colon",
        about: "SGR in colon form: colours with and without a colour space, underline styles",
        status: Status::Agree,
        ratty_only: false,
        generate: sgr_colon,
    },
    Family {
        name: "sgr-invalid",
        about: "SGR with an invalid or short colour, then more attributes",
        status: Status::Decided(
            "an invalid or short colour, which ITU-T T.416 (13.1.8) does not settle, reads as xterm reads it: 38;5;300 and 38;2;256;0;0 are no colour in xterm, alacritty and tmux, but index 44 and black in Ghostty, libvterm, avt and xterm.js; 48;5 and 38;2;1;2, cut short, are index 0 and (1, 2, 0) in xterm and xterm.js, no colour in the rest (`replay --engines all --size 1x3 '\\e[38;5;300;1mX'`); wezterm drops the rest of the SGR",
        ),
        ratty_only: false,
        generate: sgr_invalid,
    },
    Family {
        name: "save",
        about: "DECSC, DECRC, SCOSC and SCORC",
        status: Status::Decided(
            "a soft-wrapped row written over again once DECRC or SCORC moves the cursor back into it stays soft-wrapped, as in xterm (the references know no soft wrap): xterm, Ghostty, avt, xterm.js and tmux keep it; alacritty, wezterm and libvterm end the wrap, and outvote fux-vt (`replay --engines all --size 4x9 'a' 'abcx0123456789~!@#  \\e[s   ~!@#\\e[uabcabc'`)",
        ),
        ratty_only: false,
        generate: save,
    },
    Family {
        name: "alternate",
        about: "modes 47, 1047, 1048 and 1049",
        status: Status::Differs(
            "fux-vt switches screens as xterm does (its ctlseqs; against xterm alone it differs only where xterm's jump scroll drops a line scrolled in the same write as a switch), and the panel splits: 1049's clear ends a pending wrap in xterm and wezterm, as ED's does (DEC STD 070, Appendix D.6.1) and fux-vt's does, where Ghostty, alacritty, libvterm, avt, xterm.js and tmux keep it (`replay --engines all --size 2x3 'abc' '\\e[?1049hc'`); alacritty does not implement 47 or 1047, nor libvterm 47 (`cases mode-1047-and-1048`; `replay --engines all --size 4x5 '\\e[2;3r\\e[?6h\\e[?47h\\e[1;1HX'`)",
        ),
        ratty_only: false,
        generate: alternate,
    },
    Family {
        name: "modes",
        about: "DECCKM, DECTCEM, bracketed paste, focus reporting, DECKPAM and DECKPNM",
        status: Status::Agree,
        ratty_only: false,
        generate: modes,
    },
    Family {
        name: "tabs",
        about: "HT, HTS, TBC, CHT and CBT",
        status: Status::Differs(
            "the panel splits on CBT with a wrap pending: ECMA-48 (8.3.7) moves back a tab stop; xterm moves its cursor but keeps the wrap pending, so the next glyph wraps, and xterm.js leaves the cursor; fux-vt leaves the cursor in the last column, so text lands where xterm puts it (a recorded departure), while Ghostty, alacritty, libvterm, avt and wezterm move back and outvote fux-vt (`replay --engines all --size 2x10 'abcdefghij\\e[ZX'`)",
        ),
        ratty_only: false,
        generate: tabs,
    },
    Family {
        name: "charset",
        about: "DEC special graphics: designating G0 and G1, SO and SI, line-drawing letters",
        status: Status::Agree,
        ratty_only: false,
        generate: charset,
    },
    Family {
        name: "repeat",
        about: "REP (CSI b) after a glyph",
        status: Status::Differs(
            "the panel splits on a REP right after another: ECMA-48 (8.3.103) leaves REP undefined after a control function, and xterm, xterm.js and tmux repeat nothing then, as fux-vt does, where Ghostty, alacritty, libvterm, avt and wezterm repeat the glyph again and outvote fux-vt (`replay --engines all --size 1x8 '-\\e[2b\\e[2b'`). Also the one-column split of `wide`",
        ),
        ratty_only: false,
        generate: repeat,
    },
    Family {
        name: "titles",
        about: "OSC 0, 1 and 2, ended by BEL or ST",
        status: Status::Agree,
        ratty_only: false,
        generate: titles,
    },
    Family {
        name: "strings",
        about: "DCS, APC, PM and SOS strings with ASCII and UTF-8 payloads, then text",
        status: Status::Agree,
        ratty_only: false,
        generate: strings,
    },
    Family {
        name: "strings-c1",
        about: "DCS, APC, PM and SOS strings whose UTF-8 payload holds the byte 0x9c (8-bit ST)",
        status: Status::Differs(
            "fux-vt ends a DCS string at the byte (F6); Ghostty ends DCS, SOS, PM and APC strings at it and prints U+FFFD for the rest of the character. In a UTF-8 terminal neither should",
        ),
        ratty_only: false,
        generate: strings_c1,
    },
    Family {
        name: "reset",
        about: "RIS and DECSTR",
        status: Status::Decided(
            "DECSTR shows the cursor and resets DECCKM, as the VT520 manual (DECSTR, Table 5-6) and DEC STD 070 (p. 4-37) say and xterm does, and turns autowrap on, as xterm does where both say off (a departure: fux-vt/README.md). The panel splits on each: cursor visibility (xterm, libvterm, avt and xterm.js reset it; Ghostty, alacritty, wezterm and tmux keep it), DECCKM (xterm, libvterm, wezterm and xterm.js reset it; Ghostty, alacritty, avt and tmux keep it) and autowrap (xterm, libvterm, wezterm and xterm.js turn it on; Ghostty, alacritty, avt and tmux keep it off), so once `modes` or `autowrap` has set them the default panel outvotes fux-vt (`replay --engines all --size 1x1 '\\e[?25l\\e[!p'`)",
        ),
        ratty_only: false,
        generate: reset,
    },
    Family {
        name: "kitty",
        about: "kitty keyboard flags: push, pop and set",
        status: Status::Agree,
        ratty_only: true,
        generate: kitty,
    },
    Family {
        name: "reports",
        about: "DSR 5n and 6n",
        status: Status::Agree,
        // Without an identity, fux-vt reports a cursor waiting to wrap one
        // past the last column, as the vt100 crate did.
        ratty_only: true,
        generate: reports,
    },
    Family {
        name: "insert-mode",
        about: "IRM on and off",
        status: Status::Agree,
        ratty_only: false,
        generate: insert_mode,
    },
    Family {
        name: "invalid-utf8",
        about: "invalid and truncated UTF-8, and U+FFFD itself",
        status: Status::Differs(
            "invalid UTF-8 and U+FFFD are dropped; Ghostty, xterm and VTE print U+FFFD (F9)",
        ),
        ratty_only: false,
        generate: invalid_utf8,
    },
    Family {
        name: "hostile",
        about: "random bytes from an alphabet of sequence pieces",
        status: Status::Differs(
            "mixes pieces of every family, so it differs wherever any of them does",
        ),
        ratty_only: false,
        generate: hostile,
    },
    Family {
        name: RESIZE,
        about: "resizes between pieces of output, reflowing the primary screen",
        status: Status::Differs(
            "where the cursor's reflowed line no longer fits on the screen, Ghostty reflows the columns at the old height first and adds rows below, so the cursor can lose its character; fux-vt keeps the cursor on its character and pulls rows back from history. Random cases also settle the cursor first (see Case::newline_before_resize) for the choices each makes on purpose",
        ),
        ratty_only: true,
        generate: resize,
    },
];

pub fn find(name: &str) -> Option<usize> {
    FAMILIES.iter().position(|f| f.name == name)
}
