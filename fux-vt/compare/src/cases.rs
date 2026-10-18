//! Named cases: one sequence each, the smallest that shows a behaviour,
//! in the family it belongs to. Most come from the audit of fux-vt 0.2.0.
//! A case must agree, in whatever family, unless a standard or xterm
//! decides its family (judged as `case::judged` says) or it is recorded as
//! differing (`DIFFERING`), and then only where its record says, for as
//! long as it still differs there.

/// Name, family, size (rows, columns), steps: output in `escape` form, or
/// `resize:RxC`.
pub type Named = (
    &'static str,
    &'static str,
    (u16, u16),
    &'static [&'static str],
);

pub const CASES: &[Named] = &[
    // Synchronized output ends with DECSTR in fux-vt and xterm.js, so that
    // `tput init` and `tput reset` never leave a frame waiting; Ghostty
    // keeps it. The spec is silent.
    (
        "decstr-ends-synchronized-output",
        "sync",
        (1, 4),
        &["\\e[?2026h", "\\e[!p"],
    ),
    // Hyperlinks (OSC 8, references/modern/osc8_hyperlinks.md): a link
    // closed; a link without an id is a link of its own, so two runs to one
    // URI are two links (VTE's way, which the spec asks multiplexers to
    // take); the same id and URI again are one link; a wide glyph keeps its
    // link. wezterm merges the two runs, and tells no links apart.
    (
        "a-link-printed-and-closed",
        "links",
        (1, 6),
        &["\\e]8;;http://a.example/\\e\\\\ab\\e]8;;\\e\\\\c"],
    ),
    (
        "two-links-without-an-id-to-one-uri-are-two",
        "links",
        (1, 6),
        &[
            "\\e]8;;http://a.example/\\e\\\\ab\\e]8;;\\e\\\\ \\e]8;;http://a.example/\\e\\\\cd\\e]8;;\\e\\\\",
        ],
    ),
    (
        "the-same-id-twice-is-one-link",
        "links",
        (1, 6),
        &[
            "\\e]8;id=1;http://a.example/\\e\\\\ab\\e]8;;\\e\\\\ \\e]8;id=1;http://a.example/\\e\\\\cd",
        ],
    ),
    (
        "a-wide-glyph-keeps-its-link",
        "links",
        (1, 4),
        &["\\e]8;;http://a.example/\\e\\\\\\u{754c}x"],
    ),
    // Where the engines that keep links split (the family's reason).
    (
        "a-uri-outside-printable-ascii-opens-no-link",
        "link-edges",
        (1, 4),
        &["\\e]8;;http://\\u{e4}.example/\\e\\\\ab"],
    ),
    (
        "an-empty-id-is-no-id",
        "link-edges",
        (1, 4),
        &["\\e]8;id=;u\\e\\\\a\\e]8;id=;u\\e\\\\b"],
    ),
    (
        "the-blank-before-a-wide-glyph-that-wraps-has-no-link",
        "link-edges",
        (2, 3),
        &["\\e]8;;u\\e\\\\ab\\u{754c}"],
    ),
    (
        "decrc-restores-no-link",
        "link-edges",
        (1, 4),
        &["\\e]8;;u\\e\\\\a\\e7\\e]8;;\\e\\\\\\e8d"],
    ),
    // Recorded verdicts (families with `Status::Decided`): each pins one
    // point, and `verdicts` checks it beside the family's deciding engines
    // (xterm, but for `prompts` and `invalid-utf8`), or, for a family no
    // engine follows all through (`by` empty: its cases the points where
    // xterm departs from the standard), runs the family's test instead.
    //
    // Prompt marks (OSC 133): A does a fresh line, then marks the row; L
    // does the fresh line alone; ED 2 erases the marks with the rows.
    (
        "a-prompt-mark-starts-a-fresh-line",
        "prompts",
        (3, 6),
        &["out\\e]133;A\\e\\\\$ "],
    ),
    (
        "l-starts-a-fresh-line",
        "prompts",
        (3, 6),
        &["out\\e]133;L\\e\\\\x"],
    ),
    (
        "ed-2-clears-prompt-marks",
        "prompts",
        (3, 6),
        &[
            "\\e]133;A\\e\\\\$ ls\\r\\n\\e]133;C\\e\\\\out\\r\\n\\e]133;D;0\\e\\\\\\e]133;A\\e\\\\$ ",
            "\\e[H\\e[2J",
        ],
    ),
    (
        "tab-ends-a-pending-wrap",
        "pending-wrap",
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
        "resetting-autowrap-ends-a-pending-wrap",
        "pending-wrap",
        (2, 5),
        &["abcde\\e[?7l\\e[?7hX"],
    ),
    (
        "no-wrap-is-left-pending-with-autowrap-off",
        "pending-wrap",
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
    (
        "ls2-locks-g2-into-gl",
        "charset-g2-g3",
        (1, 4),
        &["\\e*0\\enqq\\x0fq"],
    ),
    (
        "ss3-shifts-g3-for-one-character",
        "charset-g2-g3",
        (1, 4),
        &["\\e+0\\eOxx"],
    ),
    ("rep-repeats-the-last-glyph", "repeat", (1, 8), &["-\\e[4b"]),
    (
        "rep-after-rep-repeats-nothing",
        "repeat",
        (1, 8),
        &["-\\e[2b\\e[2b"],
    ),
    (
        "rep-after-a-cluster-repeats-its-base",
        "repeat",
        (1, 8),
        &["e\\u{301}\\e[2b"],
    ),
    (
        "decstbm-homes-to-the-origin",
        "scroll",
        (5, 5),
        &["abc\\e[3;5rX"],
    ),
    (
        "decstbm-ignores-an-invalid-region",
        "scroll",
        (5, 5),
        &["\\e[2;4r\\e[3;3H\\e[4;2rX"],
    ),
    (
        "hpa-hpr-and-vpr",
        "cursor",
        (5, 8),
        &["\\e[2;4r\\e[2;2H\\e[5eX\\e[3`Y\\e[2aZ"],
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
        "cbt-while-wrap-pending",
        "tabs",
        (2, 10),
        &["abcdefghij\\e[ZX"],
    ),
    (
        "invalid-utf8-prints-a-replacement",
        "invalid-utf8",
        (1, 10),
        &["caf\\xe9 ok"],
    ),
    (
        "a-continuation-byte-alone-prints-a-replacement",
        "invalid-utf8",
        (1, 12),
        &["price \\xa35"],
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

/// A named case recorded as differing, in a family that does not differ
/// by a decision: the fields it differs on (a difference's key, `*` for
/// any run of characters), and why. It fails on any other field, and when
/// it no longer differs there. Every other named case must agree.
pub struct Differing {
    pub case: &'static str,
    pub on: &'static [&'static str],
    pub why: &'static str,
}

pub const DIFFERING: &[Differing] = &[Differing {
    case: "reflow-moves-the-saved-cursor",
    on: &["cell (*) text", "cursor", "row * soft-wrapped"],
    why: "the resize family's: where the cursor's reflowed line no longer fits, Ghostty reflows the columns at the old height first; fux-vt keeps the cursor on its character and pulls rows back from history (wezterm and xterm.js agree with fux-vt)",
}];

/// The named case `name`'s record, if it is recorded as differing.
pub fn differing(name: &str) -> Option<&'static Differing> {
    DIFFERING.iter().find(|d| d.case == name)
}

/// What the section a decided family cites says one of its named cases
/// leaves, after its last step: each field by the key [`read`] takes and
/// the value a difference in it shows (`row Y` its text). The deciding
/// engines (`by`) judge only the fields the section leaves open, so an
/// engine that departs from the section never outvotes it, and a family no
/// engine follows is still compared, with its section.
///
/// `quote` is the passage the values are derived from, by hand: never
/// from fux-vt's output. `section` names where it is, with its edition.
///
/// [`read`]: crate::snapshot::read
pub struct Expected {
    pub case: &'static str,
    pub section: &'static str,
    pub quote: &'static str,
    pub fields: &'static [(&'static str, &'static str)],
}

const D61: &str = "DEC STD 070 (1991), Appendix D.6.1";
const D61_FLAG: &str = "writing a character into the last position in a line causes a special flag (referred to as the Last Column Flag) to be set ... and if Auto Wrap Mode is set, will cause the Active position to advance to the first column of the next line PRIOR to entering that character. The flag is then reset. The Last Column Flag should also be reset by executing any control which potentially changes the Active Position from the end-of-line position";
const SGR: &str = "ECMA-48 (5th edition, 1991), 8.3.117";
const OSC8: &str = "references/modern/osc8_hyperlinks.md, \"The escape sequence\"";
const OSC8_PAINTED: &str = "all subsequent cells that are painted are hyperlinks to this target. A hyperlink is closed with the same escape sequence, omitting the parameters and the URI";
const DECSTR: &str = "DEC STD 070 (1991), Soft Terminal Reset (DECSTR)";

/// The expected fields of every named case in a family a standard
/// decides (`Authority::Standard`).
pub const EXPECTED: &[Expected] = &[
    Expected {
        case: "tab-ends-a-pending-wrap",
        section: D61,
        quote: "HORIZONTAL_TAB is among the controls that reset the Last Column Flag; HT: \"If no tabulation stop is reached before the Right Margin ..., the Active Position will be set to the Right Margin\" (DEC STD 070, HT, note 1)",
        fields: &[
            ("row 0", "abcdX"),
            ("row 1", ""),
            ("cursor", "(0, 4)"),
            ("pending wrap", "true"),
        ],
    },
    Expected {
        case: "resetting-autowrap-ends-a-pending-wrap",
        section: D61,
        quote: "RESET_MODE (... AUTO_WRAP_MODE) is among the controls that reset the Last Column Flag",
        fields: &[
            ("row 0", "abcdX"),
            ("row 1", ""),
            ("cursor", "(0, 4)"),
            ("pending wrap", "true"),
        ],
    },
    Expected {
        case: "no-wrap-is-left-pending-with-autowrap-off",
        section: D61,
        quote: "When Auto Wrap Mode is set, however, writing a character into the last position in a line causes a special flag ... to be set",
        fields: &[
            ("row 0", "cX"),
            ("row 1", ""),
            ("cursor", "(0, 1)"),
            ("pending wrap", "true"),
        ],
    },
    Expected {
        case: "backspace-while-wrap-pending",
        section: D61,
        quote: D61_FLAG,
        fields: &[
            ("row 0", "abcXe"),
            ("cursor", "(0, 4)"),
            ("pending wrap", "false"),
        ],
    },
    Expected {
        case: "cub-while-wrap-pending",
        section: D61,
        quote: D61_FLAG,
        fields: &[
            ("row 0", "abcXe"),
            ("cursor", "(0, 4)"),
            ("pending wrap", "false"),
        ],
    },
    Expected {
        case: "el-while-wrap-pending",
        section: D61,
        quote: D61_FLAG,
        fields: &[
            ("row 0", "abcd"),
            ("cursor", "(0, 4)"),
            ("pending wrap", "false"),
        ],
    },
    Expected {
        case: "ech-while-wrap-pending",
        section: D61,
        quote: D61_FLAG,
        fields: &[
            ("row 0", "abcd"),
            ("cursor", "(0, 4)"),
            ("pending wrap", "false"),
        ],
    },
    Expected {
        case: "ich-while-wrap-pending",
        section: D61,
        quote: D61_FLAG,
        fields: &[
            ("row 0", "abcd"),
            ("cursor", "(0, 4)"),
            ("pending wrap", "false"),
        ],
    },
    Expected {
        case: "lf-while-wrap-pending",
        section: D61,
        quote: D61_FLAG,
        fields: &[
            ("row 0", "abcde"),
            ("row 1", "    X"),
            ("cursor", "(1, 4)"),
            ("pending wrap", "true"),
        ],
    },
    Expected {
        case: "cuu-while-wrap-pending",
        section: D61,
        quote: D61_FLAG,
        fields: &[
            ("row 0", "zzzzX"),
            ("row 1", "abcde"),
            ("cursor", "(0, 4)"),
            ("pending wrap", "true"),
        ],
    },
    Expected {
        case: "ri-while-wrap-pending",
        section: D61,
        quote: D61_FLAG,
        fields: &[
            ("row 0", "zzzzX"),
            ("row 1", "abcde"),
            ("cursor", "(0, 4)"),
            ("pending wrap", "true"),
        ],
    },
    Expected {
        case: "tab-then-print-in-last-column",
        section: D61,
        quote: D61_FLAG,
        fields: &[
            ("row 0", "   YZ"),
            ("cursor", "(0, 4)"),
            ("pending wrap", "false"),
        ],
    },
    Expected {
        case: "a-line-feed-that-scrolls-ends-a-pending-wrap",
        section: D61,
        quote: D61_FLAG,
        fields: &[
            ("row 0", "def"),
            ("row 1", "  X"),
            ("cursor", "(1, 2)"),
            ("pending wrap", "true"),
        ],
    },
    Expected {
        case: "ed-ends-a-pending-wrap",
        section: D61,
        quote: "ERASE IN DISPLAY is among the controls that reset the Last Column Flag",
        fields: &[
            ("row 0", "abcdX"),
            ("row 1", ""),
            ("cursor", "(0, 4)"),
            ("pending wrap", "true"),
        ],
    },
    Expected {
        case: "ind-and-nel",
        section: D61,
        quote: "INDEX and NEXT_LINE are among the controls that reset the Last Column Flag",
        fields: &[
            ("row 0", "ab"),
            ("row 1", "  X"),
            ("row 2", "Y"),
            ("cursor", "(2, 1)"),
        ],
    },
    Expected {
        case: "il-moves-to-column-zero",
        section: "DEC STD 070 (1991), IL, note 2",
        quote: "The Active Position is set to the Left Margin in the active line after execution of this control. The active line does not change.",
        fields: &[
            ("row 0", "X"),
            ("row 1", "ab"),
            ("row 2", ""),
            ("cursor", "(0, 1)"),
        ],
    },
    Expected {
        case: "dl-moves-to-column-zero",
        section: "DEC STD 070 (1991), DL, note 2",
        quote: "The Active Position is set to the Left Margin in the active line after execution of this control. The active line does not change.",
        fields: &[
            ("row 0", "Yd"),
            ("row 1", ""),
            ("row 2", ""),
            ("cursor", "(0, 1)"),
        ],
    },
    Expected {
        case: "bold-and-faint-together",
        section: SGR,
        quote: "22 normal colour or normal intensity (neither bold nor faint)",
        fields: &[
            ("row 0", "XY"),
            ("cell (0,1) bold", "false"),
            ("cell (0,1) dim", "false"),
        ],
    },
    Expected {
        case: "sgr-21-underlines-beside-bold",
        section: SGR,
        quote: "1 bold or increased intensity; 21 doubly underlined; 24 not underlined (neither singly nor doubly)",
        fields: &[
            ("cell (0,0) bold", "true"),
            ("cell (0,0) underline", "true"),
            ("cell (0,1) bold", "true"),
            ("cell (0,1) underline", "false"),
        ],
    },
    Expected {
        case: "sgr-hidden-strikeout-blink",
        section: SGR,
        quote: "5 slowly blinking; 8 concealed characters; 9 crossed-out; 28 revealed characters; 29 not crossed out",
        fields: &[
            ("row 0", "ABC"),
            ("cell (0,0) hidden", "true"),
            ("cell (0,0) strikeout", "false"),
            ("cell (0,1) hidden", "false"),
            ("cell (0,1) strikeout", "true"),
            ("cell (0,1) blink", "false"),
            ("cell (0,2) hidden", "false"),
            ("cell (0,2) strikeout", "false"),
            ("cell (0,2) blink", "true"),
        ],
    },
    Expected {
        // SGR 58 is "reserved for future standardization" in ECMA-48: the
        // underline colour is left to the deciders.
        case: "sgr-underline-colour",
        section: SGR,
        quote: "4 singly underlined",
        fields: &[("row 0", "X"), ("cell (0,0) underline", "true")],
    },
    Expected {
        case: "hts-and-tbc",
        section: "ECMA-48 (5th edition, 1991), 8.3.154 (TBC), 8.3.62 (HTS) and 8.3.60 (HT)",
        quote: "3 all character tabulation stops are cleared; HTS causes a character tabulation stop to be set at the active presentation position; HT causes the active presentation position to be moved to the following character tabulation stop",
        fields: &[("row 0", "X   Y"), ("cursor", "(0, 5)")],
    },
    Expected {
        // The stops are DEC's at power-up, every eighth column from the
        // ninth (DEC STD 070, RIS's algorithm: `x%8 == 1`).
        case: "cht-and-cbt",
        section: "ECMA-48 (5th edition, 1991), 8.3.10 (CHT) and 8.3.7 (CBT)",
        quote: "CHT causes the active presentation position to be moved to the character position corresponding to the n-th following character tabulation stop; CBT ... the n-th preceding character tabulation stop",
        fields: &[("row 0", "                Y"), ("cursor", "(0, 17)")],
    },
    Expected {
        case: "cbt-while-wrap-pending",
        section: "ECMA-48 (5th edition, 1991), 8.3.7; DEC STD 070 (1991), Appendix D.6.1",
        quote: "CBT causes the active presentation position to be moved to the character position corresponding to the n-th preceding character tabulation stop; the Last Column Flag should also be reset by executing any control which potentially changes the Active Position from the end-of-line position",
        fields: &[
            ("row 0", "abcdefghXj"),
            ("row 1", ""),
            ("cursor", "(0, 9)"),
            ("pending wrap", "false"),
        ],
    },
    Expected {
        case: "ls2-locks-g2-into-gl",
        section: "ECMA-35 (6th edition, 1994), 9.3.1",
        quote: "Each of the four locking-shift functions SI, SO, LS2, and LS3 shall invoke in the GL area the specific graphic character set that has been identified as the respective code element G0, G1, G2, or G3",
        fields: &[("row 0", "\u{2500}\u{2500}q"), ("cursor", "(0, 3)")],
    },
    Expected {
        case: "ss3-shifts-g3-for-one-character",
        section: "ECMA-35 (6th edition, 1994), 9.4",
        quote: "the immediately following bit combination ... shall represent the corresponding character from the set that is designated as the code element G2 or G3 respectively. The shift status that was in effect immediately before the occurrence of the single-shift function shall continue in effect",
        fields: &[("row 0", "\u{2502}x"), ("cursor", "(0, 2)")],
    },
    Expected {
        case: "dcs-payload-holding-0x9c",
        section: "ECMA-48 (5th edition, 1991), 8.3.143; in UTF-8, 0x9c is a byte of U+271C, not ST",
        quote: "ST is used as the closing delimiter of a control string opened by ... DEVICE CONTROL STRING (DCS)",
        fields: &[("row 0", "ok"), ("cursor", "(0, 2)")],
    },
    Expected {
        case: "invalid-utf8-prints-a-replacement",
        section: "the Unicode Standard 17.0, 3.9, U+FFFD Substitution of Maximal Subparts",
        quote: "an ill-formed subsequence ... is replaced by a single U+FFFD for each maximal subpart",
        fields: &[("row 0", "caf\u{fffd} ok"), ("cursor", "(0, 7)")],
    },
    Expected {
        case: "a-continuation-byte-alone-prints-a-replacement",
        section: "the Unicode Standard 17.0, 3.9, U+FFFD Substitution of Maximal Subparts",
        quote: "an ill-formed subsequence ... is replaced by a single U+FFFD for each maximal subpart",
        fields: &[("row 0", "price \u{fffd}5"), ("cursor", "(0, 8)")],
    },
    Expected {
        case: "decstr-turns-autowrap-on",
        section: DECSTR,
        quote: "Auto Wrap Mode: Auto Wrap Off (NVM if present); read as the terminal's setting, which is on",
        fields: &[
            ("autowrap", "true"),
            ("row 0", "abc"),
            ("row 1", "d"),
            ("cursor", "(1, 1)"),
        ],
    },
    Expected {
        case: "decstr-shows-the-cursor-and-resets-cursor-keys",
        section: DECSTR,
        quote: "Cursor On/Off: on (ignore NVM); Cursor Key Mode: reset (Cursor)",
        fields: &[("cursor visible", "true"), ("application cursor", "false")],
    },
    Expected {
        case: "decstr-keeps-the-screen",
        section: DECSTR,
        quote: "Current Rendition: normal; the following are also NOT affected by the soft reset: The data in the display; DECSTR has no affect on the Active Position",
        fields: &[
            ("row 0", "abX"),
            ("cursor", "(0, 3)"),
            ("cell (0,2) bold", "false"),
        ],
    },
    Expected {
        case: "a-uri-outside-printable-ascii-opens-no-link",
        section: OSC8,
        quote: "the parameters and the URI must not contain any bytes outside of the 32-126 range. If they do, the behavior is undefined (\"Encodings\"): the link is left to no one, the text is printed",
        fields: &[("row 0", "ab"), ("cursor", "(0, 2)")],
    },
    Expected {
        case: "an-empty-id-is-no-id",
        section: OSC8,
        quote: "all subsequent cells that are painted are hyperlinks to this target; which cells without an id (or with an empty one, \"interchangeable\") belong together is left to heuristics (\"Hover underlining and the id parameter\")",
        fields: &[
            ("row 0", "ab"),
            ("cell (0,0) link", "Some(\"u\")"),
            ("cell (0,1) link", "Some(\"u\")"),
        ],
    },
    Expected {
        case: "the-blank-before-a-wide-glyph-that-wraps-has-no-link",
        section: OSC8,
        quote: OSC8_PAINTED,
        fields: &[
            ("row 0", "ab"),
            ("cell (0,0) link", "Some(\"u\")"),
            ("cell (0,1) link", "Some(\"u\")"),
            ("cell (0,2) link", "None"),
        ],
    },
    Expected {
        case: "decrc-restores-no-link",
        section: OSC8,
        quote: OSC8_PAINTED,
        fields: &[
            ("row 0", "ad"),
            ("cell (0,0) link", "Some(\"u\")"),
            ("cell (0,1) link", "None"),
        ],
    },
    Expected {
        case: "a-prompt-mark-starts-a-fresh-line",
        section: "references/modern/osc133_semantic_prompts.md, \"Commands\"",
        quote: "OSC \"133;A\": First do a fresh-line. Then start a new command, and enter prompt mode; fresh-line: If the cursor is the initial column, do nothing. Otherwise, do the equivalent of \"\\r\\n\"",
        fields: &[
            ("row 0", "out"),
            ("row 1", "$"),
            ("row 1 starts a prompt", "true"),
            ("cursor", "(1, 2)"),
        ],
    },
    Expected {
        case: "l-starts-a-fresh-line",
        section: "references/modern/osc133_semantic_prompts.md, \"Commands\"",
        quote: "OSC \"133;L\": Do a fresh-line: If the cursor is the initial column, do nothing. Otherwise, do the equivalent of \"\\r\\n\"",
        fields: &[("row 0", "out"), ("row 1", "x"), ("cursor", "(1, 1)")],
    },
    Expected {
        // The proposal says nothing of ED and marks: Ghostty and wezterm
        // decide whether the marks go with the rows.
        case: "ed-2-clears-prompt-marks",
        section: "ECMA-48 (5th edition, 1991), 8.3.39",
        quote: "2 all character positions of the page are put into the erased state",
        fields: &[
            ("row 0", ""),
            ("row 1", ""),
            ("row 2", ""),
            ("cursor", "(0, 0)"),
        ],
    },
];

/// The expected fields of the named case `name`, if a standard decides
/// its family.
pub fn expected(name: &str) -> Option<&'static Expected> {
    EXPECTED.iter().find(|e| e.case == name)
}

#[cfg(test)]
mod tests {
    use super::{CASES, EXPECTED};
    use crate::families::{self, Authority, FAMILIES, Status};
    use crate::snapshot::{Cell, Line, Snapshot, Style, Width};

    /// Every named case of a family a standard decides has its expected
    /// fields, once, and only those cases have them; every field is one a
    /// screen of the case's size has.
    #[test]
    fn every_case_a_standard_decides_has_its_expected_fields() {
        for &(name, family, (rows, cols), _) in CASES {
            let status = families::find(family)
                .and_then(|i| FAMILIES.get(i))
                .map(|f| f.status);
            let standard = matches!(
                status,
                Some(Status::Decided {
                    authority: Authority::Standard { .. },
                    ..
                })
            );
            let found: Vec<_> = EXPECTED.iter().filter(|e| e.case == name).collect();
            assert_eq!(found.len(), usize::from(standard), "{name}");
            let blank = Line {
                cells: vec![Cell::new("", Width::Narrow, Style::default()); usize::from(cols)],
                wrapped: false,
                prompt: false,
                unread_from: None,
            };
            let screen = Snapshot {
                rows,
                cols,
                screen: vec![blank; usize::from(rows)],
                ..Snapshot::default()
            };
            for e in found {
                assert!(!e.fields.is_empty() && !e.quote.is_empty(), "{name}");
                for (key, _) in e.fields {
                    assert!(
                        crate::snapshot::read(&screen, key).is_some(),
                        "{name}: {key}"
                    );
                }
            }
        }
        for e in EXPECTED {
            assert!(CASES.iter().any(|c| c.0 == e.case), "{}", e.case);
        }
    }
}
