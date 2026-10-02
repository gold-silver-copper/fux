//! Named cases: one sequence each, the smallest that shows a behaviour,
//! in the family it belongs to. Most come from the audit of fux-vt 0.2.0.
//! A case in a family expected to agree must agree; a case in a family
//! that differs is reported either way, so a fix shows as it lands.

/// Name, family, size (rows, columns), steps: output in `escape` form, or
/// `resize:RxC`.
pub type Named = (
    &'static str,
    &'static str,
    (u16, u16),
    &'static [&'static str],
);

pub const CASES: &[Named] = &[
    // Recorded verdicts (families with `Status::Decided`): each pins one
    // point, and `verdicts` checks it beside xterm.
    (
        "tab-keeps-a-pending-wrap",
        "controls",
        (2, 5),
        &["abcde\\tX"],
    ),
    (
        "a-line-feed-that-scrolls-ends-a-pending-wrap",
        "controls",
        (2, 3),
        &["abcdef\\nX"],
    ),
    ("ed-ends-a-pending-wrap", "erase", (2, 5), &["abcde\\e[JX"]),
    (
        "resetting-autowrap-keeps-a-pending-wrap",
        "autowrap",
        (2, 5),
        &["abcde\\e[?7l\\e[?7hX"],
    ),
    (
        "a-wrap-is-left-pending-with-autowrap-off",
        "autowrap",
        (2, 2),
        &["\\e[?7lca\\e[?7hX"],
    ),
    (
        "bold-and-faint-together",
        "sgr",
        (1, 3),
        &["\\e[1m\\e[2mX\\e[22mY"],
    ),
    (
        "sgr-21-underlines-beside-bold",
        "sgr",
        (1, 3),
        &["\\e[1m\\e[21mX\\e[24mY"],
    ),
    (
        "sgr-colour-cut-short",
        "sgr-invalid",
        (1, 3),
        &["\\e[48;5mX\\e[0;38;2;1;2mY"],
    ),
    (
        "a-row-rewritten-after-decrc-stays-soft-wrapped",
        "save",
        (4, 9),
        &["a", "abcx0123456789~!@#  \\e[s   ~!@#\\e[uabcabc"],
    ),
    (
        "decstr-turns-autowrap-on",
        "reset",
        (2, 3),
        &["\\e[?7l\\e[!pabcd"],
    ),
    (
        "decstr-shows-the-cursor-and-resets-cursor-keys",
        "reset",
        (1, 1),
        &["\\e[?25l\\e[?1h\\e[!p"],
    ),
    (
        "backspace-while-wrap-pending",
        "pending-wrap",
        (1, 5),
        &["abcde\\x08X"],
    ),
    (
        "cub-while-wrap-pending",
        "pending-wrap",
        (1, 5),
        &["abcde\\e[DX"],
    ),
    (
        "el-while-wrap-pending",
        "pending-wrap",
        (1, 5),
        &["abcde\\e[K"],
    ),
    (
        "ech-while-wrap-pending",
        "pending-wrap",
        (1, 5),
        &["abcde\\e[X"],
    ),
    (
        "ich-while-wrap-pending",
        "pending-wrap",
        (1, 5),
        &["abcde\\e[@"],
    ),
    (
        "lf-while-wrap-pending",
        "pending-wrap",
        (2, 5),
        &["abcde\\nX"],
    ),
    (
        "cuu-while-wrap-pending",
        "pending-wrap",
        (2, 5),
        &["zzzzz\\r\\nabcde\\e[AX"],
    ),
    (
        "ri-while-wrap-pending",
        "pending-wrap",
        (2, 5),
        &["zzzzz\\r\\nabcde\\eMX"],
    ),
    (
        "tab-then-print-in-last-column",
        "pending-wrap",
        (1, 5),
        &["\\tZ\\x08Y"],
    ),
    (
        "sgr-colon-rgb-with-colour-space",
        "sgr-colon",
        (1, 3),
        &["\\e[38:2::255:0:0mX"],
    ),
    (
        "sgr-colon-underline-off",
        "sgr-colon",
        (1, 3),
        &["\\e[4m\\e[4:0mX"],
    ),
    (
        "sgr-invalid-index-keeps-the-rest",
        "sgr-invalid",
        (1, 3),
        &["\\e[38;5;300;1mX"],
    ),
    (
        "sgr-hidden-strikeout-blink",
        "sgr",
        (1, 4),
        &["\\e[8mA\\e[28;9mB\\e[29;5mC"],
    ),
    ("sgr-underline-colour", "sgr", (1, 3), &["\\e[4;58;5;9mX"]),
    ("dec-line-drawing", "charset", (1, 6), &["\\e(0lqqk\\e(Bx"]),
    (
        "shift-out-to-g1",
        "charset",
        (1, 6),
        &["\\e)0\\x0elqk\\x0fq"],
    ),
    ("rep-repeats-the-last-glyph", "repeat", (1, 8), &["-\\e[4b"]),
    (
        "decstbm-homes-to-the-origin",
        "scroll",
        (5, 5),
        &["abc\\e[3;5rX"],
    ),
    (
        "vpa-honours-origin-mode",
        "origin",
        (5, 5),
        &["\\e[2;4r\\e[?6h\\e[2dX"],
    ),
    ("il-moves-to-column-zero", "edit", (3, 5), &["ab\\e[LX"]),
    (
        "dl-moves-to-column-zero",
        "edit",
        (3, 5),
        &["ab\\r\\ncd\\e[AX\\e[MY"],
    ),
    ("ind-and-nel", "index", (3, 5), &["ab\\eDX\\eEY"]),
    (
        "hts-and-tbc",
        "tabs",
        (1, 20),
        &["\\e[3g\\e[5G\\eH\\rX\\tY"],
    ),
    ("cht-and-cbt", "tabs", (1, 20), &["\\e[2IX\\e[ZY"]),
    (
        "invalid-utf8-prints-a-replacement",
        "invalid-utf8",
        (1, 10),
        &["caf\\xe9 ok"],
    ),
    (
        "dcs-payload-holding-0x9c",
        "strings-c1",
        (1, 12),
        &["\\eP1$r\\u{271c} leaked\\e\\\\ok"],
    ),
    (
        "wide-glyph-wrapping-marks-the-row",
        "wide",
        (2, 5),
        &["\\u{3042}\\u{3044}\\u{3046}"],
    ),
    (
        "wide-glyph-wrapping-at-four-columns",
        "wide",
        (2, 4),
        &["abc\\u{754c}x"],
    ),
    (
        "mode-47-keeps-the-cursor",
        "alternate",
        (2, 5),
        &["ab\\e[?47hX"],
    ),
    (
        "mode-1049-keeps-the-cursor",
        "alternate",
        (2, 5),
        &["ab\\e[?1049hX"],
    ),
    (
        "mode-1047-and-1048",
        "alternate",
        (2, 5),
        &["ab\\e[?1048h\\e[?1047hX\\e[?1047l\\e[?1048lY"],
    ),
    (
        "insert-mode",
        "insert-mode",
        (1, 6),
        &["abc\\r\\e[4hX\\e[4lY"],
    ),
    ("title", "titles", (1, 4), &["\\e]2;hello\\x07"]),
    (
        "cursor-report-while-wrap-pending",
        "reports",
        (1, 5),
        &["abcde\\e[6n"],
    ),
    ("kitty-flags", "kitty", (1, 4), &["\\e[>1u\\e[>3u\\e[<u"]),
    (
        "decstr-keeps-the-screen",
        "reset",
        (2, 5),
        &["ab\\e[1m\\e[!pX"],
    ),
    (
        "reflow-moves-the-saved-cursor",
        "resize",
        (8, 20),
        &[
            "aaaaaaaaaaaaaaaaaa\\r\\nbbbbbbbbbbbbbbbbbb\\r\\n$ \\e[?1049h",
            "resize:8x10",
            "\\e[?1049l",
        ],
    ),
    (
        "reflow-narrower-and-back",
        "resize",
        (3, 8),
        &["abcdefgh12", "resize:3x4", "resize:3x8"],
    ),
];
