//! Families of output a program writes, each with what it exercises and
//! whether fux-vt and Ghostty are expected to agree on it. A random run
//! draws only from families expected to agree, so any difference it finds
//! is a regression; `--family` and `--all` add the others.
use crate::rng::Rng;

/// Whether the two terminals are expected to agree on a family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Agree,
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
            "on a one-column screen a wide glyph is dropped without moving the cursor, as xterm and tmux do and fux-vt's README says; Ghostty, alacritty, libvterm, avt and wezterm draw it or wrap it, and outvote fux-vt (`replay --engines all --size 2x1 '\\u{754c}X'`)",
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
        status: Status::Differs(
            "the panel splits on a line feed (LF, VT, FF) that scrolls while a wrap is pending: xterm, Ghostty, wezterm and xterm.js end the wrap, as DEC STD 070 (Appendix D.6.1) says and fux-vt does; alacritty, libvterm and avt keep it, and outvote fux-vt (`replay --engines all --size 2x3 'abcdef\\nX'`)",
        ),
        ratty_only: false,
        generate: controls,
    },
    Family {
        name: "cursor",
        about: "CUU CUD CUF CUB CNL CPL CHA VPA HPA HPR VPR, CUP and HVP",
        status: Status::Differs(
            "HPA (CSI `), HPR (CSI a) and VPR (CSI e) are not implemented (F8)",
        ),
        ratty_only: false,
        generate: cursor,
    },
    Family {
        name: "erase",
        about: "ED, EL and ECH",
        status: Status::Differs(
            "the panel splits: ED, EL and ECH end a pending wrap, as DEC STD 070 (Appendix D.6.1) says and xterm, Ghostty and fux-vt do; alacritty, libvterm and avt keep it, and outvote fux-vt (`replay --engines all --size 1x5 'abcde\\e[KX'`)",
        ),
        ratty_only: false,
        generate: erase,
    },
    Family {
        name: "edit",
        about: "ICH, DCH, IL and DL",
        status: Status::Differs(
            "IL and DL keep the cursor's column (F8); and the panel splits on ICH and DCH, which end a pending wrap in xterm and Ghostty, as DEC STD 070 (Appendix D.6.1) says and fux-vt does, where alacritty and libvterm keep it (`replay --engines all --size 1x5 'abcde\\e[@X'`)",
        ),
        ratty_only: false,
        generate: edit,
    },
    Family {
        name: "scroll",
        about: "SU, SD, DECSTBM and RI",
        status: Status::Differs(
            "DECSTBM homes to the top margin with DECOM off, and resets an invalid region instead of ignoring it (F8); SD keeps a moved row's soft-wrap flag, Ghostty clears it",
        ),
        ratty_only: false,
        generate: scroll,
    },
    Family {
        name: "index",
        about: "IND (ESC D) and NEL (ESC E)",
        status: Status::Differs("IND (ESC D) and NEL (ESC E) are not implemented"),
        ratty_only: false,
        generate: index,
    },
    Family {
        name: "origin",
        about: "DECOM with a scroll region, then cursor movement",
        status: Status::Differs(
            "VPA ignores origin mode (F8), and DECSTBM's differences (see scroll)",
        ),
        ratty_only: false,
        generate: origin,
    },
    Family {
        name: "autowrap",
        about: "DECAWM on and off",
        status: Status::Differs(
            "the panel splits, and fux-vt is right: with DECAWM off, a glyph in the last column leaves a wrap pending, which fires once DECAWM is set again. xterm, xterm.js, Ghostty and alacritty agree with fux-vt; libvterm, avt, wezterm and tmux leave none, so the default panel outvotes fux-vt (`replay --engines all --size 1x2 '\\e[?7lca\\e[?7h '`)",
        ),
        ratty_only: false,
        generate: autowrap,
    },
    Family {
        name: "pending-wrap",
        about: "a glyph in the last column, then a control, movement or edit while the wrap is pending",
        status: Status::Differs(
            "the panel splits: EL, ECH, ICH, DCH and a line feed that scrolls end a pending wrap in xterm and Ghostty, as DEC STD 070 (Appendix D.6.1) says and fux-vt does; alacritty, libvterm and avt keep it, and outvote fux-vt (`replay --engines all --size 1x5 'abcde\\e[KX'`). Also IL and DL keep the cursor's column (F8), and the one-column split of `wide`",
        ),
        ratty_only: false,
        generate: pending_wrap,
    },
    Family {
        name: "sgr",
        about: "SGR in semicolon form: every attribute, its reset, 16, 256 and RGB colours, underline colour",
        status: Status::Differs(
            "the panel splits where SGR 21 or 2 follows bold, and fux-vt does as xterm does: xterm, Ghostty, libvterm, xterm.js and tmux keep bold beside the underline (21) and dim (2); alacritty and avt take 21 as bold off, and avt and wezterm let dim replace bold, so together they outvote fux-vt (`replay --engines all --size 1x3 '\\e[1m\\e[21;2mX'`)",
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
        status: Status::Differs(
            "the panel splits on invalid colours, and fux-vt reads them as xterm does: 38;5;300 and 38;2;256;0;0 are no colour in xterm, alacritty and tmux, but index 44 and black in Ghostty, libvterm, avt and xterm.js; 48;5 and 38;2;1;2, cut short, are index 0 and (1, 2, 0) in xterm and xterm.js, no colour in the rest (`replay --engines all --size 1x3 '\\e[38;5;300;1mX'`); wezterm drops the rest of the SGR",
        ),
        ratty_only: false,
        generate: sgr_invalid,
    },
    Family {
        name: "save",
        about: "DECSC, DECRC, SCOSC and SCORC",
        status: Status::Differs(
            "the panel splits on a soft-wrapped row written over again once DECRC or SCORC moves the cursor back into it: xterm, Ghostty, avt, xterm.js and tmux keep the row soft-wrapped, as fux-vt does; alacritty, wezterm and libvterm end the wrap, and outvote fux-vt (`replay --engines all --size 4x9 'a' 'abcx0123456789~!@#  \\e[s   ~!@#\\e[uabcabc'`)",
        ),
        ratty_only: false,
        generate: save,
    },
    Family {
        name: "alternate",
        about: "modes 47, 1047, 1048 and 1049",
        status: Status::Differs(
            "1047 and 1048 are not implemented (documented), and 47 and 1049 home the cursor instead of keeping it (F10)",
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
            "tab stops are fixed every eight columns: HTS, TBC, CHT and CBT are not implemented (documented)",
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
        status: Status::Differs("REP (CSI b) is not implemented (F7)"),
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
        status: Status::Differs(
            "the panel splits on what DECSTR resets, and fux-vt resets what xterm resets (VT520 manual p. 5-150): cursor visibility (xterm, libvterm, avt and xterm.js reset it; Ghostty, alacritty, wezterm and tmux keep it) and DECCKM (xterm, libvterm, wezterm and xterm.js reset it; Ghostty, alacritty, avt and tmux keep it), so once `modes` has set them the default panel outvotes fux-vt (`replay --engines all --size 1x1 '\\e[?25l\\e[!p'`). Alone, with text, it agrees",
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
        status: Status::Differs(
            "IRM (CSI 4 h) is not implemented: printing overwrites instead of inserting",
        ),
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
