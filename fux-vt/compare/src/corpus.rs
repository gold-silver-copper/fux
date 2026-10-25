//! The corpus: what real programs wrote to a terminal, recorded by
//! `record` (`corpus/record.sh` records every scenario), and replayed here
//! through fux-vt beside other engines.
//!
//! A recording is replayed as a case of its own size, one step for each
//! step recorded: the program starting, then the output after each line
//! of keys. fux-vt is set up as fux sets up a pane (reflow, the kitty
//! keyboard protocol, no identity), as the program was recorded, with
//! fux's 10000 rows of history. After each step the engines are compared
//! with fux-vt field by field, as in `run`.
//!
//! **xterm decides what it can tell.** Where xterm runs, any field it can
//! tell that differs from fux-vt fails the step. A field xterm cannot tell
//! (underline colour, pending wrap, the kitty flags), and every field once
//! xterm abstains (it does from SGR 58 on, see its file), is decided by the
//! vote of the other engines, as in `run`. A difference the other engines
//! outvote fux-vt on, where xterm agrees with fux-vt, is shown, not failed.
//!
//! **A resize is judged by the program's answer to it.** A step that
//! resized the terminal is two steps of the case: the resize, and what the
//! program wrote after it. The engines are compared after both, but only
//! the second is judged. Right after the resize, before the program has
//! redrawn, each engine shows its own way of resizing (whether it reflows,
//! what comes back from history, where the cursor lands), which they choose
//! differently on purpose, as `run` avoids by settling the cursor before
//! each resize; fux's panes reflow, as the panel does and xterm does not. What the
//! program draws for its new size is what is judged. What it leaves as the
//! resize left it (a shell's earlier lines, history) stays as each engine
//! resized it, and a recording where that differs has the reason in its
//! status.
//!
//! Each recording has its exceptions (`STATUSES`): none where it is
//! expected to agree, else each difference it is expected to show, by the
//! recorded steps it shows at, the fields, and why. The comparison goes on
//! past an exception to the recording's end, and `corpus` fails on a
//! difference no exception covers, on an exception that no longer occurs,
//! and on a recording with no status.
use crate::case::{self, Case, Snippet, Step, Verdict};
use crate::engine::{self, ENGINES, FUX_VT};
use crate::snapshot::{self, Field};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Where the recordings are.
pub fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus")
}

/// The reason Claude Code's recordings differ at their last step.
const CLAUDE_TITLE: &str = "Claude Code sets an empty title as it exits (OSC 0 ;). \
    xterm then shows its default title, `xterm` (a quirk of xterm's, in its file), and \
    fux-vt keeps the empty title. Every step before the last agrees beside xterm. \
    `corpus --engines xterm,panel claude` shows it.";

/// The reason the recordings with emoji sequences differ, from the start.
const CLUSTERS: &str = "An emoji sequence (a modifier, a ZWJ sequence, a flag, VS16) is one \
    wide cell in fux-vt, as in Ghostty and wezterm: the `clusters` family's recorded choice. \
    xterm keeps each code point a cell of its own (and VS16 narrow), and alacritty, libvterm \
    and avt mostly do. helix and less lay a line out by clusters as fux-vt does (Ghostty and \
    wezterm agree with fux-vt on helix-unicode and less-small); vim, neovim and micro by code \
    points, each placed by a cursor move of its own, so their emoji differ in every engine, \
    Ghostty joining a modifier printed after a cursor move to the cell before it. \
    `corpus --engines xterm,ghostty,wezterm vim-unicode` shows it.";

/// The reason zsh-resize differs.
const REFLOW: &str = "fux's panes reflow (fux::pane::OPTIONS), as Ghostty, wezterm and \
    libvterm do: at the shrink to 20x60, zsh's line typed before it is rewrapped onto two \
    rows with the cursor on the second, and zsh redraws its prompt from there, so the line \
    shows twice, as in those three. xterm, alacritty and avt, as run here, cut the line \
    instead. Every step before the shrink agrees. `corpus zsh-resize` shows it.";

/// The reason tmux-resize differs.
const ALTERNATE_RESIZE: &str = "Leaving the alternate screen after it shrank, with the cursor \
    low, and grew again, xterm puts the cursor where the alternate screen's moved to, fux-vt \
    and the panel where 1049 saved it, so tmux's `[exited]` lands on another row: a choice of \
    xterm's resize (`replay --engines xterm,ghostty,alacritty --size 40x120 --history 10000 \
    --no-reflow '\\e[?1049h\\e[34;1H' resize:10x40 resize:40x120 '\\e[?1049lX'`). Every step \
    before the last agrees.";

/// A difference a recording is expected to show: where, on what, and why.
pub struct Exception {
    /// The recorded steps it shows at, first and last, counted as a
    /// report counts them ("after step N of M": 1 is the program's start).
    pub steps: (usize, usize),
    /// The fields, as a difference's key (`cell (3,4) text`, `title`),
    /// with `*` standing for any run of characters.
    pub field: &'static str,
    pub reason: &'static str,
}

impl Exception {
    /// Whether it covers a failure on `key` after the recorded step `step`.
    pub fn covers(&self, step: usize, key: &str) -> bool {
        (self.steps.0..=self.steps.1).contains(&step) && crate::families::glob(self.field, key)
    }
}

/// claude's title, at its last step.
const CLAUDE_AT: &[Exception] = &[Exception {
    steps: (3, 3),
    field: "title",
    reason: CLAUDE_TITLE,
}];

/// claude-resize's title, at its last step.
const CLAUDE_RESIZE_AT: &[Exception] = &[Exception {
    steps: (5, 5),
    field: "title",
    reason: CLAUDE_TITLE,
}];

/// helix-unicode's clusters, from its start: the cells and the styles they move.
const HELIX_UNICODE_AT: &[Exception] = &[
    Exception {
        steps: (1, 11),
        field: "cell (*) text",
        reason: CLUSTERS,
    },
    Exception {
        steps: (1, 11),
        field: "cell (*) width",
        reason: CLUSTERS,
    },
    Exception {
        steps: (1, 11),
        field: "cell (*) fg",
        reason: CLUSTERS,
    },
    Exception {
        steps: (1, 11),
        field: "cell (*) bg",
        reason: CLUSTERS,
    },
];

/// neovim pushes the kitty keyboard flags 1 and 2 (report event types)
/// and reads them back; fux-vt keeps only the flags it honours, not 2, as
/// the kitty spec's "Detection of support" asks (84fd3fc3), where
/// Ghostty, alacritty and wezterm, which send release events, keep it.
const KITTY_FLAGS_KEPT: &str = "fux-vt keeps only the kitty flags it honours, 1 and not report event types (2), as the kitty spec's detection of support asks; Ghostty, alacritty and wezterm keep 2";

const NVIM_DIAGNOSTICS_AT: &[Exception] = &[Exception {
    steps: (1, 9),
    field: "kitty keyboard flags",
    reason: KITTY_FLAGS_KEPT,
}];

const NVIM_DIFF_AT: &[Exception] = &[Exception {
    steps: (1, 8),
    field: "kitty keyboard flags",
    reason: KITTY_FLAGS_KEPT,
}];

const NVIM_HELP_AT: &[Exception] = &[Exception {
    steps: (1, 7),
    field: "kitty keyboard flags",
    reason: KITTY_FLAGS_KEPT,
}];

const NVIM_INSERT_AT: &[Exception] = &[Exception {
    steps: (1, 13),
    field: "kitty keyboard flags",
    reason: KITTY_FLAGS_KEPT,
}];

const NVIM_NETRW_AT: &[Exception] = &[Exception {
    steps: (1, 8),
    field: "kitty keyboard flags",
    reason: KITTY_FLAGS_KEPT,
}];

const NVIM_RESIZE_AT: &[Exception] = &[Exception {
    steps: (1, 9),
    field: "kitty keyboard flags",
    reason: KITTY_FLAGS_KEPT,
}];

const NVIM_SCROLL_AT: &[Exception] = &[Exception {
    steps: (1, 10),
    field: "kitty keyboard flags",
    reason: KITTY_FLAGS_KEPT,
}];

const NVIM_SEARCH_AT: &[Exception] = &[Exception {
    steps: (1, 10),
    field: "kitty keyboard flags",
    reason: KITTY_FLAGS_KEPT,
}];

const NVIM_SMALL_AT: &[Exception] = &[Exception {
    steps: (1, 8),
    field: "kitty keyboard flags",
    reason: KITTY_FLAGS_KEPT,
}];

const NVIM_SPLIT_AT: &[Exception] = &[Exception {
    steps: (1, 9),
    field: "kitty keyboard flags",
    reason: KITTY_FLAGS_KEPT,
}];

const NVIM_TABS_AT: &[Exception] = &[Exception {
    steps: (1, 7),
    field: "kitty keyboard flags",
    reason: KITTY_FLAGS_KEPT,
}];

const NVIM_TERMINAL_AT: &[Exception] = &[Exception {
    steps: (1, 9),
    field: "kitty keyboard flags",
    reason: KITTY_FLAGS_KEPT,
}];

const NVIM_VISUAL_AT: &[Exception] = &[Exception {
    steps: (1, 18),
    field: "kitty keyboard flags",
    reason: KITTY_FLAGS_KEPT,
}];

const NVIM_WIDE_AT: &[Exception] = &[Exception {
    steps: (1, 6),
    field: "kitty keyboard flags",
    reason: KITTY_FLAGS_KEPT,
}];

/// nvim-unicode's clusters, from its start: the cells and the styles they move.
const NVIM_UNICODE_AT: &[Exception] = &[
    Exception {
        steps: (1, 12),
        field: "kitty keyboard flags",
        reason: KITTY_FLAGS_KEPT,
    },
    Exception {
        steps: (1, 11),
        field: "cell (*) text",
        reason: CLUSTERS,
    },
    Exception {
        steps: (1, 11),
        field: "cell (*) width",
        reason: CLUSTERS,
    },
    Exception {
        steps: (1, 11),
        field: "cell (*) fg",
        reason: CLUSTERS,
    },
    Exception {
        steps: (1, 11),
        field: "cell (*) bg",
        reason: CLUSTERS,
    },
];

/// vim-unicode's clusters, from its start.
const VIM_UNICODE_AT: &[Exception] = &[
    Exception {
        steps: (1, 11),
        field: "cell (*) text",
        reason: CLUSTERS,
    },
    Exception {
        steps: (1, 11),
        field: "cell (*) width",
        reason: CLUSTERS,
    },
];

/// micro-small's clusters, from its start: the cells and the styles they move.
const MICRO_SMALL_AT: &[Exception] = &[
    Exception {
        steps: (1, 6),
        field: "cell (*) text",
        reason: CLUSTERS,
    },
    Exception {
        steps: (1, 6),
        field: "cell (*) width",
        reason: CLUSTERS,
    },
    Exception {
        steps: (1, 6),
        field: "cell (*) fg",
        reason: CLUSTERS,
    },
    Exception {
        steps: (1, 6),
        field: "cell (*) bg",
        reason: CLUSTERS,
    },
];

/// less-small's clusters, on the pages that show them.
const LESS_SMALL_AT: &[Exception] = &[
    Exception {
        steps: (1, 3),
        field: "cell (*) text",
        reason: CLUSTERS,
    },
    Exception {
        steps: (1, 3),
        field: "cell (*) width",
        reason: CLUSTERS,
    },
    Exception {
        steps: (6, 6),
        field: "cell (*) text",
        reason: CLUSTERS,
    },
    Exception {
        steps: (6, 6),
        field: "cell (*) width",
        reason: CLUSTERS,
    },
];

/// tmux-resize's `[exited]`, at its last step.
const TMUX_RESIZE_AT: &[Exception] = &[
    Exception {
        steps: (12, 12),
        field: "cell (*) text",
        reason: ALTERNATE_RESIZE,
    },
    Exception {
        steps: (12, 12),
        field: "cursor",
        reason: ALTERNATE_RESIZE,
    },
];

/// zsh-resize's line shown twice after each shrink, and the history its reflow keeps.
const ZSH_RESIZE_AT: &[Exception] = &[
    Exception {
        steps: (3, 4),
        field: "cell (*) text",
        reason: REFLOW,
    },
    Exception {
        steps: (3, 4),
        field: "cell (*) fg",
        reason: REFLOW,
    },
    Exception {
        steps: (3, 4),
        field: "cursor",
        reason: REFLOW,
    },
    Exception {
        steps: (3, 4),
        field: "row 1 soft-wrapped",
        reason: REFLOW,
    },
    Exception {
        steps: (8, 8),
        field: "cell (*) text",
        reason: REFLOW,
    },
    Exception {
        steps: (8, 8),
        field: "cell (*) fg",
        reason: REFLOW,
    },
    Exception {
        steps: (8, 8),
        field: "cursor",
        reason: REFLOW,
    },
    Exception {
        steps: (8, 9),
        field: "history row *",
        reason: REFLOW,
    },
    Exception {
        steps: (8, 9),
        field: "history rows kept",
        reason: REFLOW,
    },
];

/// How fux-vt's replies have changed since the recordings were made: a
/// reply as recorded (`was`), as fux-vt gives it now, and the change that
/// made it, cited. Each is applied to every recording's replies before
/// they are compared with fux-vt's now (`record::replies_now`), so any
/// other change to DA, DECRQM or the colour replies shows in the corpus;
/// one that no recording shows any more fails.
pub const REPLY_CHANGES: &[(&str, &str, &str)] = &[
    (
        "\x1b[?62;22c",
        "\x1b[?62;6;21;22;28c",
        "DA1 names selective erase (6), horizontal scrolling (21) and rectangular editing (28): a218350c and the commits before it",
    ),
    (
        "\x1b[?2027;0$y",
        "\x1b[?2027;3$y",
        "mode 2027 (grapheme clusters) is permanently set: 3a6bda6a",
    ),
    (
        "\x1b[?69;0$y",
        "\x1b[?69;2$y",
        "DECLRMM (69) is a mode fux-vt knows, reset: f9a18914",
    ),
    (
        "\x1b[?1016;0$y",
        "\x1b[?1016;4$y",
        "SGR-Pixels mouse mode (1016) is a mode fux-vt knows, permanently reset while the host says no cell size, as no report in pixels can be made then",
    ),
];

/// The replies that came with XTGETTCAP's answer and XTQMODKEYS's.
const XTGETTCAP_AND_XTQMODKEYS: &str = "XTGETTCAP is answered, as an invalid request (82983a1d), and XTQMODKEYS (bdf426c5), which went unanswered when it was recorded";

/// The recordings whose replies changed in a way no reply as recorded can
/// say (a reply given now to a query once left unanswered), and why. One
/// that agrees again fails.
pub const REPLIES_DIFFER: &[(&str, &str)] = &[
    ("fish", XTGETTCAP_AND_XTQMODKEYS),
    ("fish-complete", XTGETTCAP_AND_XTQMODKEYS),
    ("fish-history", XTGETTCAP_AND_XTQMODKEYS),
    ("fish-small", XTGETTCAP_AND_XTQMODKEYS),
    ("lazygit", XTGETTCAP_AND_XTQMODKEYS),
    ("lazygit-small", XTGETTCAP_AND_XTQMODKEYS),
    ("lazygit-stage", XTGETTCAP_AND_XTQMODKEYS),
    ("nvim-diagnostics", XTGETTCAP_AND_XTQMODKEYS),
    ("nvim-diff", XTGETTCAP_AND_XTQMODKEYS),
    ("nvim-help", XTGETTCAP_AND_XTQMODKEYS),
    ("nvim-insert", XTGETTCAP_AND_XTQMODKEYS),
    ("nvim-netrw", XTGETTCAP_AND_XTQMODKEYS),
    ("nvim-resize", XTGETTCAP_AND_XTQMODKEYS),
    ("nvim-scroll", XTGETTCAP_AND_XTQMODKEYS),
    ("nvim-search", XTGETTCAP_AND_XTQMODKEYS),
    ("nvim-small", XTGETTCAP_AND_XTQMODKEYS),
    ("nvim-split", XTGETTCAP_AND_XTQMODKEYS),
    ("nvim-tabs", XTGETTCAP_AND_XTQMODKEYS),
    ("nvim-terminal", XTGETTCAP_AND_XTQMODKEYS),
    ("nvim-unicode", XTGETTCAP_AND_XTQMODKEYS),
    ("nvim-visual", XTGETTCAP_AND_XTQMODKEYS),
    ("nvim-wide", XTGETTCAP_AND_XTQMODKEYS),
    ("vim", XTGETTCAP_AND_XTQMODKEYS),
    ("vim-diff", XTGETTCAP_AND_XTQMODKEYS),
    ("vim-help", XTGETTCAP_AND_XTQMODKEYS),
    ("vim-insert", XTGETTCAP_AND_XTQMODKEYS),
    ("vim-resize", XTGETTCAP_AND_XTQMODKEYS),
    ("vim-small", XTGETTCAP_AND_XTQMODKEYS),
    ("vim-terminal", XTGETTCAP_AND_XTQMODKEYS),
    ("vim-unicode", XTGETTCAP_AND_XTQMODKEYS),
    (
        "zellij",
        "zellij asks OSC 4 for each of its 256 colours; fux's panes answer them now (Feature::Palette, src/pane.rs), where they answered none when it was recorded",
    ),
    (
        "zellij-small",
        "zellij asks OSC 4 for each of its 256 colours; fux's panes answer them now (Feature::Palette, src/pane.rs), where they answered none when it was recorded",
    ),
];

/// fux-vt's replies `now` beside those a recording kept (`then`), as
/// they are compared: versions as `VERSION`, and the recorded replies with
/// every change since ([`REPLY_CHANGES`]) applied; and which changes were.
fn replies_beside(now: &[u8], then: &[u8]) -> (String, String, Vec<usize>) {
    let now = unversioned(now);
    let mut then = unversioned(then);
    let mut used = Vec::new();
    for (i, (was, is, _)) in REPLY_CHANGES.iter().enumerate() {
        if then.contains(was) {
            used.push(i);
            then = then.replace(was, is);
        }
    }
    (now, then, used)
}

/// A reply's version numbers, which change with every release, as
/// `VERSION`: XTVERSION's `fux X.Y.Z` and DA2's version parameter. What a
/// reply says otherwise is compared.
pub fn unversioned(replies: &[u8]) -> String {
    let text = String::from_utf8_lossy(replies).into_owned();
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(at) = [rest.find("fux "), rest.find("\x1b[>1;")]
        .into_iter()
        .flatten()
        .min()
    {
        let (head, tail) = rest.split_at_checked(at).unwrap_or((rest, ""));
        out.push_str(head);
        let (lead, digits) = if let Some(t) = tail.strip_prefix("fux ") {
            ("fux ", t)
        } else {
            ("\x1b[>1;", tail.get(5..).unwrap_or_default())
        };
        out.push_str(lead);
        out.push_str("VERSION");
        let end = digits
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(digits.len());
        rest = digits.get(end..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

/// No exception: the recording is expected to agree.
const AGREE: &[Exception] = &[];

/// What is expected of each recording: its exceptions, none if it is to
/// agree. One not listed here is reported as new, and fails.
pub const STATUSES: &[(&str, &[Exception])] = &[
    ("bash", AGREE),
    ("bash-complete", AGREE),
    ("bash-history", AGREE),
    ("bash-small", AGREE),
    ("bat-diff", AGREE),
    ("bat-markdown", AGREE),
    ("bat-page", AGREE),
    ("btop", AGREE),
    ("btop-small", AGREE),
    ("cargo-build", AGREE),
    ("cargo-errors", AGREE),
    ("cargo-test", AGREE),
    ("clang-errors", AGREE),
    ("claude", CLAUDE_AT),
    ("claude-ghostty", CLAUDE_AT),
    ("claude-main", CLAUDE_AT),
    ("claude-resize", CLAUDE_RESIZE_AT),
    ("claude-small", CLAUDE_AT),
    ("delta-diff", AGREE),
    ("delta-log", AGREE),
    ("delta-show", AGREE),
    ("delta-wide", AGREE),
    ("emacs-dired", AGREE),
    ("emacs-mx", AGREE),
    ("emacs-resize", AGREE),
    ("emacs-scroll", AGREE),
    ("emacs-split", AGREE),
    ("fish", AGREE),
    ("fish-complete", AGREE),
    ("fish-history", AGREE),
    ("fish-small", AGREE),
    ("fzf", AGREE),
    ("fzf-height", AGREE),
    ("fzf-multi", AGREE),
    ("fzf-preview", AGREE),
    ("fzf-small", AGREE),
    ("git-add-p", AGREE),
    ("git-diff", AGREE),
    ("git-graph", AGREE),
    ("gls", AGREE),
    ("gls-long", AGREE),
    ("gls-wide", AGREE),
    ("helix", AGREE),
    ("helix-picker", AGREE),
    ("helix-resize", AGREE),
    ("helix-select", AGREE),
    ("helix-small", AGREE),
    ("helix-unicode", HELIX_UNICODE_AT),
    ("htop", AGREE),
    ("htop-small", AGREE),
    ("htop-tree", AGREE),
    ("lazygit", AGREE),
    ("lazygit-small", AGREE),
    ("lazygit-stage", AGREE),
    ("less", AGREE),
    ("less-chop", AGREE),
    ("less-color", AGREE),
    ("less-small", LESS_SMALL_AT),
    ("man", AGREE),
    ("man-long", AGREE),
    ("man-small", AGREE),
    ("man-tables", AGREE),
    ("man-wide", AGREE),
    ("mc", AGREE),
    ("mc-small", AGREE),
    ("micro-edit", AGREE),
    ("micro-small", MICRO_SMALL_AT),
    ("micro-split", AGREE),
    ("ncdu", AGREE),
    ("nnn", AGREE),
    ("nnn-detail", AGREE),
    ("npm-install", AGREE),
    ("nvim-diagnostics", NVIM_DIAGNOSTICS_AT),
    ("nvim-diff", NVIM_DIFF_AT),
    ("nvim-help", NVIM_HELP_AT),
    ("nvim-insert", NVIM_INSERT_AT),
    ("nvim-netrw", NVIM_NETRW_AT),
    ("nvim-resize", NVIM_RESIZE_AT),
    ("nvim-scroll", NVIM_SCROLL_AT),
    ("nvim-search", NVIM_SEARCH_AT),
    ("nvim-small", NVIM_SMALL_AT),
    ("nvim-split", NVIM_SPLIT_AT),
    ("nvim-tabs", NVIM_TABS_AT),
    ("nvim-terminal", NVIM_TERMINAL_AT),
    ("nvim-unicode", NVIM_UNICODE_AT),
    ("nvim-visual", NVIM_VISUAL_AT),
    ("nvim-wide", NVIM_WIDE_AT),
    ("pico", AGREE),
    ("ranger", AGREE),
    ("tig", AGREE),
    ("tig-blame", AGREE),
    ("tig-tree", AGREE),
    ("tmux", AGREE),
    ("tmux-copy", AGREE),
    ("tmux-resize", TMUX_RESIZE_AT),
    ("tmux-small", AGREE),
    ("tmux-vim", AGREE),
    ("top", AGREE),
    ("vim", AGREE),
    ("vim-diff", AGREE),
    ("vim-help", AGREE),
    ("vim-insert", AGREE),
    ("vim-resize", AGREE),
    ("vim-small", AGREE),
    ("vim-terminal", AGREE),
    ("vim-unicode", VIM_UNICODE_AT),
    ("zellij", AGREE),
    ("zellij-small", AGREE),
    ("zsh", AGREE),
    ("zsh-history", AGREE),
    ("zsh-menu", AGREE),
    ("zsh-resize", ZSH_RESIZE_AT),
    ("zsh-small", AGREE),
];

/// A recording, as `record` saved it.
pub struct Recording {
    pub name: String,
    pub program: String,
    pub version: String,
    pub rows: u16,
    pub cols: u16,
    /// Each step's keys (escaped, as typed) and the output after them.
    pub steps: Vec<(String, Vec<u8>)>,
    /// What fux-vt answered the program while it was recorded, in order
    /// (`record`), escaped in the JSON's `replies`.
    pub replies: Vec<u8>,
    /// The steps that resized the terminal (`!resize` in the keys) rather
    /// than typing: the step's index and the new size, rows and columns.
    /// The step's output is what the program wrote after the resize.
    pub resizes: Vec<(usize, u16, u16)>,
    /// `--chunk N`: each step's output given, and judged, N bytes at a
    /// time, not whole (none: whole).
    pub chunk: Option<usize>,
}

impl Recording {
    /// Every byte the program wrote, in order.
    pub fn bytes(&self) -> Vec<u8> {
        self.steps
            .iter()
            .flat_map(|(_, bytes)| bytes.iter().copied())
            .collect()
    }

    /// What the program wrote, at each size the terminal had as it wrote
    /// it: from its first size, a new one at each step that resized it.
    /// fux-bench's `Recording::segments` makes the same.
    pub fn segments(&self) -> Vec<(u16, u16, Vec<u8>)> {
        let mut out = Vec::new();
        let (mut rows, mut cols, mut bytes) = (self.rows, self.cols, Vec::new());
        for (i, (_, output)) in self.steps.iter().enumerate() {
            if let Some((r, c)) = self.resize_at(i) {
                out.push((rows, cols, std::mem::take(&mut bytes)));
                (rows, cols) = (r, c);
            }
            bytes.extend_from_slice(output);
        }
        out.push((rows, cols, bytes));
        out
    }

    /// The size the terminal is resized to before step `index`'s output,
    /// if that step resized it.
    pub fn resize_at(&self, index: usize) -> Option<(u16, u16)> {
        self.resizes
            .iter()
            .find(|(i, _, _)| *i == index)
            .map(|&(_, rows, cols)| (rows, cols))
    }

    /// Step `index`'s output in the pieces the case gives it: whole, or
    /// [`Recording::chunk`] bytes at a time; one, empty, for none.
    fn pieces(&self, index: usize) -> Vec<Vec<u8>> {
        let output = self.steps.get(index).map_or(&[][..], |(_, b)| b.as_slice());
        match self.chunk.filter(|&n| n > 0 && output.len() > n) {
            None => vec![output.to_vec()],
            Some(n) => {
                let (mut out, mut rest) = (Vec::new(), output);
                while !rest.is_empty() {
                    let (piece, later) = rest.split_at_checked(n).unwrap_or((rest, &[]));
                    out.push(piece.to_vec());
                    rest = later;
                }
                out
            }
        }
    }

    /// The step recorded (counted from 0) that the case's step `index`
    /// belongs to, as `Outcome::step` counts them: none for 0, before any
    /// step, the program's start for 1. A step that resized is two in the
    /// case, the resize and the output after it (or more, one for each
    /// piece of its output under `--chunk`).
    pub fn recorded_step(&self, index: usize) -> Option<usize> {
        let mut case_steps = 0usize;
        for i in 0..self.steps.len() {
            let resized = usize::from(self.resize_at(i).is_some());
            let these = resized.saturating_add(self.pieces(i).len());
            case_steps = case_steps.saturating_add(these);
            if index > 0 && case_steps >= index {
                return Some(i);
            }
        }
        None
    }

    /// Whether each comparison the case makes is judged, in order: one
    /// before any step, then one after each. All are but those right after
    /// a resize, before the program's answer to it (see the module
    /// documentation).
    pub fn judged(&self) -> Vec<bool> {
        std::iter::once(true)
            .chain((0..self.steps.len()).flat_map(|i| {
                let resized = self.resize_at(i).map(|_| false);
                resized
                    .into_iter()
                    .chain(std::iter::repeat_n(true, self.pieces(i).len()))
            }))
            .collect()
    }

    /// The recording as a case: one step for each step recorded, and a
    /// resize before the output of each step that resized.
    pub fn case(&self) -> Case {
        Case {
            rows: self.rows,
            cols: self.cols,
            history: 10_000,
            reflow: fux::pane::OPTIONS.has(fux_vt::Feature::Reflow),
            newline_before_resize: false,
            steps: self
                .steps
                .iter()
                .enumerate()
                .flat_map(|(i, _)| {
                    let resize = self
                        .resize_at(i)
                        .map(|(rows, cols)| Step::Resize(rows, cols));
                    resize
                        .into_iter()
                        .chain(self.pieces(i).into_iter().map(|bytes| {
                            Step::Output(vec![Snippet {
                                family: usize::MAX,
                                bytes,
                            }])
                        }))
                })
                .collect(),
            subject: FUX_VT,
        }
    }
}

fn field<'a>(
    value: &'a serde_json::Value,
    name: &str,
    path: &Path,
) -> Result<&'a serde_json::Value, String> {
    value
        .get(name)
        .ok_or(format!("{}: no {name:?}", path.display()))
}

fn load(path: &Path) -> Result<Recording, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let bytes = std::fs::read(path.with_extension("bin"))
        .map_err(|e| format!("{}: {e}", path.with_extension("bin").display()))?;
    let string = |name: &str| -> Result<String, String> {
        Ok(field(&json, name, path)?
            .as_str()
            .unwrap_or_default()
            .to_owned())
    };
    let size = field(&json, "size", path)?;
    let dimension = |i: usize| -> Result<u16, String> {
        size.get(i)
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u16::try_from(n).ok())
            .ok_or(format!("{}: a bad size", path.display()))
    };
    let mut steps = Vec::new();
    let mut resizes = Vec::new();
    let mut from = 0usize;
    for (i, step) in field(&json, "steps", path)?
        .as_array()
        .ok_or(format!("{}: steps is not a list", path.display()))?
        .iter()
        .enumerate()
    {
        if let Some(size) = step.get("resize") {
            let dimension = |d: usize| -> Result<u16, String> {
                size.get(d)
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|n| u16::try_from(n).ok())
                    .filter(|&n| n > 0)
                    .ok_or(format!("{}: a bad resize", path.display()))
            };
            resizes.push((i, dimension(0)?, dimension(1)?));
        }
        let keys = field(step, "keys", path)?
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let end = field(step, "end", path)?
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or(format!("{}: a step's end is not a number", path.display()))?;
        let output = bytes
            .get(from..end)
            .ok_or(format!("{}: a step ends past the bytes", path.display()))?;
        steps.push((keys, output.to_vec()));
        from = end;
    }
    if from != bytes.len() {
        return Err(format!("{}: bytes after the last step", path.display()));
    }
    Ok(Recording {
        name: path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        program: string("program")?,
        version: string("version")?,
        rows: dimension(0)?,
        cols: dimension(1)?,
        replies: crate::escape::unescape(&string("replies")?)
            .map_err(|e| format!("{}: replies: {e}", path.display()))?,
        steps,
        resizes,
        chunk: None,
    })
}

/// Every recording, by name, or those named.
pub fn recordings(names: &[String]) -> Result<Vec<Recording>, String> {
    let dir = dir();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    let all = paths
        .iter()
        .map(|p| load(p))
        .collect::<Result<Vec<_>, _>>()?;
    for name in names {
        if !all.iter().any(|r| &r.name == name) {
            return Err(format!("no recording {name:?} in {}", dir.display()));
        }
    }
    Ok(all
        .into_iter()
        .filter(|r| names.is_empty() || names.contains(&r.name))
        .collect())
}

/// The fields fux-vt fails on after a step: see the module documentation.
pub fn failures(verdicts: &[Verdict]) -> Vec<String> {
    let xterm = engine::find("xterm");
    let decider = verdicts.iter().find(|v| Some(v.engine) == xterm);
    let others: Vec<&Verdict> = verdicts
        .iter()
        .filter(|v| Some(v.engine) != xterm)
        .collect();
    let mut failed: BTreeSet<String> = BTreeSet::new();
    if let Some(x) = decider {
        failed.extend(x.differences.iter().map(|d| d.key.clone()));
    }
    let xterm_can = xterm.and_then(|i| ENGINES.get(i)).map(|k| k.can);
    let owned: Vec<Verdict> = others
        .iter()
        .map(|v| Verdict {
            engine: v.engine,
            differences: v.differences.clone(),
            snapshot: v.snapshot.clone(),
        })
        .collect();
    for key in case::outvoted_on(&owned) {
        let field: Option<Field> = owned
            .iter()
            .flat_map(|v| v.differences.iter())
            .find(|d| d.key == key)
            .map(|d| d.field);
        let xterm_tells = decider.is_some()
            && field.is_some_and(|f| xterm_can.is_some_and(|can| f.told_by(&can)));
        if !xterm_tells {
            failed.insert(key);
        }
    }
    failed.into_iter().collect()
}

/// [`failures`], each with what it is about.
fn failing_fields(verdicts: &[Verdict]) -> Vec<(String, Field)> {
    failures(verdicts)
        .into_iter()
        .filter_map(|key| {
            let field = verdicts
                .iter()
                .flat_map(|v| v.differences.iter())
                .find(|d| d.key == key)?
                .field;
            Some((key, field))
        })
        .collect()
}

/// Each engine's verdict at the end, as marks: `+` agrees, `-` differs,
/// `!` abstained (and why, on lines of their own).
fn marks(outcome: &case::Outcome) -> String {
    let marks = outcome
        .verdicts
        .iter()
        .map(|v| {
            let mark = if v.differences.is_empty() { '+' } else { '-' };
            format!("{mark}{}", engine::name(v.engine))
        })
        .chain(
            outcome
                .abstained
                .iter()
                .map(|(i, _)| format!("!{}", engine::name(*i))),
        )
        .collect::<Vec<_>>()
        .join(" ");
    let why: String = outcome
        .abstained
        .iter()
        .map(|(i, why)| format!("\n         {} abstains: {why}", engine::name(*i)))
        .collect();
    format!("{marks}{why}")
}

/// Where the case's step `case_step` (as `Outcome::step` counts them) is
/// in the recording: "after step N of M, keys K".
fn place(recording: &Recording, case_step: Option<usize>) -> String {
    let recorded = case_step.and_then(|s| recording.recorded_step(s));
    let step = recorded.map_or(0, |i| i.saturating_add(1));
    let keys = match recorded {
        None | Some(0) => "(start)".to_owned(),
        Some(i) => match recording.resize_at(i) {
            Some((rows, cols)) => format!("(resized to {rows}x{cols})"),
            None => recording
                .steps
                .get(i)
                .map_or(String::new(), |(k, _)| format!("'{k}'")),
        },
    };
    format!(
        "after step {step} of {}, keys {keys}",
        recording.steps.len()
    )
}

/// What went wrong at a recording's failing step: its keys, the fields,
/// each engine's differences, and the subject's screen beside the deciding
/// engine's (xterm's, or the first that differs).
fn report(recording: &Recording, subject: usize, outcome: &case::Outcome) -> String {
    let subject = engine::name(subject);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "  {}, {}: {}",
        recording.program,
        recording.version,
        place(recording, outcome.step)
    );
    let failed = failures(&outcome.verdicts);
    if !failed.is_empty() {
        let shown: Vec<&str> = failed.iter().take(6).map(String::as_str).collect();
        let more = if failed.len() > 6 { ", ..." } else { "" };
        let _ = writeln!(out, "  fails on: {}{more}", shown.join(", "));
    }
    for verdict in &outcome.verdicts {
        let name = engine::name(verdict.engine);
        if verdict.differences.is_empty() {
            let _ = writeln!(out, "    {name}: agrees");
            continue;
        }
        let _ = writeln!(out, "    {name}: differs");
        for diff in verdict.differences.iter().take(8) {
            let _ = writeln!(out, "      {}", diff.line_beside(subject, name));
        }
        if verdict.differences.len() > 8 {
            let _ = writeln!(out, "      ... ({} in all)", verdict.differences.len());
        }
    }
    let xterm = engine::find("xterm");
    let shown = outcome
        .verdicts
        .iter()
        .find(|v| Some(v.engine) == xterm && !v.differences.is_empty())
        .or_else(|| outcome.verdicts.iter().find(|v| !v.differences.is_empty()));
    if let Some(verdict) = shown {
        let name = engine::name(verdict.engine);
        let screens = snapshot::side_by_side_beside(subject, &outcome.fux, &verdict.snapshot, name);
        for line in screens.lines() {
            let _ = writeln!(out, "    {line}");
        }
    }
    out
}

fn status(name: &str) -> Option<&'static [Exception]> {
    STATUSES.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// How a recording went against its exceptions.
struct Excepted {
    /// The outcome: where the subject first fails on a field no exception
    /// covers (none: it never does, and the run went to the end).
    outcome: case::Outcome,
    /// Each failure an exception covered, by recorded step, field and the
    /// exception's index.
    covered: Vec<(usize, String, usize)>,
}

impl Excepted {
    /// Whether it agreed everywhere, no exception needed.
    fn agrees(&self) -> bool {
        self.outcome.agrees() && self.covered.is_empty()
    }

    /// The exceptions that never occurred (none, if it stopped early: a
    /// later step might have shown them).
    fn stale<'a>(&self, exceptions: &'a [Exception]) -> Vec<&'a Exception> {
        if !self.outcome.agrees() {
            return Vec::new();
        }
        exceptions
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.covered.iter().any(|(_, _, e)| e == i))
            .map(|(_, e)| e)
            .collect()
    }
}

/// Each engine that abstained in `outcome`, with its class
/// (`engine::abstention`) and why, for the JSON; and why the recording
/// went unjudged, if xterm, which decides, abstained for anything but a
/// limit of its adapter's (from then on the vote alone would judge, so the
/// run is not whole).
fn abstentions(outcome: &case::Outcome) -> (Vec<serde_json::Value>, Option<String>) {
    let xterm = engine::find("xterm");
    let mut unjudged = None;
    let each = outcome
        .abstained
        .iter()
        .map(|(i, why)| {
            let class = engine::abstention(why);
            if Some(*i) == xterm && class != "adapter-limited" {
                unjudged = Some(format!("xterm, which decides, abstained ({class}): {why}"));
            }
            serde_json::json!({ "engine": engine::name(*i), "class": class, "why": why })
        })
        .collect();
    (each, unjudged)
}

/// Sorts the failures after the recorded step `step` by the exception
/// covering each, noting each covered one in `covered`: whether any is
/// covered by none.
fn sort(
    step: usize,
    failures: Vec<String>,
    exceptions: &[Exception],
    covered: &mut Vec<(usize, String, usize)>,
) -> bool {
    let mut uncovered = false;
    for key in failures {
        match exceptions.iter().position(|e| e.covers(step, &key)) {
            Some(e) => covered.push((step, key, e)),
            None => uncovered = true,
        }
    }
    uncovered
}

/// Runs a recording's case as `corpus` judges it, through every judged
/// point: a failure an exception covers is noted and the run goes on; it
/// stops where the subject first fails on a field none covers.
fn judge_excepted(
    recording: &Recording,
    case: &Case,
    panel: &[usize],
    exceptions: &[Exception],
) -> Result<Excepted, String> {
    // `run_judged` judges once before the steps and once after each.
    let judged = recording.judged();
    let at = std::cell::Cell::new(0usize);
    let covered = std::cell::RefCell::new(Vec::new());
    let outcome = case.run_judged(panel, true, |_, v| {
        let i = at.get();
        at.set(i.saturating_add(1));
        if !judged.get(i).copied().unwrap_or(true) {
            return false;
        }
        let step = recording
            .recorded_step(i)
            .map_or(0, |r| r.saturating_add(1));
        sort(step, failures(v), exceptions, &mut covered.borrow_mut())
    })?;
    Ok(Excepted {
        outcome,
        covered: covered.into_inner(),
    })
}

/// Replays the recordings named (all if none) beside `panel`, and says how
/// each went. False if one expected to agree fails, or has no status.
/// `show` prints the subject's screen at the end of each. With a subject
/// other than fux-vt, see [`run_subject`].
pub fn run(
    subject: usize,
    panel: &[usize],
    names: &[String],
    show: bool,
    json: Option<&str>,
    chunk: Option<usize>,
) -> Result<bool, String> {
    let recordings: Vec<Recording> = (recordings(names)?.into_iter())
        .map(|r| Recording { chunk, ..r })
        .collect();
    if subject != FUX_VT {
        return run_subject(subject, panel, &recordings, !names.is_empty(), show, json);
    }
    let names_of: Vec<&str> = panel.iter().map(|&i| engine::name(i)).collect();
    println!("engines: {}", names_of.join(" "));
    let started = Instant::now();
    let mut ok = true;
    let (mut agree, mut differ) = (0usize, 0usize);
    let mut results = Vec::new();
    // The reply changes some recording shows.
    let mut shown_changes = BTreeSet::new();
    for recording in &recordings {
        let case = recording.case();
        let expected = status(&recording.name);
        let exceptions = expected.unwrap_or_default();
        // An error (an engine or the subject failing) fails the recording,
        // named; the run goes on to the others.
        let run = match judge_excepted(recording, &case, panel, exceptions) {
            Ok(run) => run,
            Err(e) => {
                ok = false;
                differ = differ.saturating_add(1);
                println!("ERROR    {}: {e}", recording.name);
                results.push(serde_json::json!({ "recording": recording.name, "error": e }));
                continue;
            }
        };
        let outcome = &run.outcome;
        let stale = run.stale(exceptions);
        let label = format!(
            "{} ({} steps, {} bytes)",
            recording.name,
            recording.steps.len(),
            recording.bytes().len()
        );
        let reasons: BTreeSet<&str> = exceptions.iter().map(|e| e.reason).collect();
        let (abstained, unjudged) = abstentions(outcome);
        // What fux-vt answers the program now, beside what it answered,
        // with the changes since it was recorded applied.
        let (now, then, used) =
            replies_beside(&crate::record::replies_now(recording)?, &recording.replies);
        shown_changes.extend(used);
        let replies_agree = now == then;
        let listed = REPLIES_DIFFER.iter().find(|(n, _)| *n == recording.name);
        match (replies_agree, listed) {
            (true, None) | (false, Some(_)) => {}
            (false, None) => {
                ok = false;
                println!(
                    "REPLIES  {label}: fux-vt answers {:?}, where it answered {:?}",
                    crate::escape::escape(now.as_bytes()),
                    crate::escape::escape(then.as_bytes())
                );
            }
            (true, Some((_, why))) => {
                ok = false;
                println!(
                    "REPLIES  {label}: listed as replying otherwise ({why}), and agrees: remove it"
                );
            }
        }
        results.push(serde_json::json!({
            // The program, so recordings of one program count once (the
            // speed method's sign test groups them).
            "program": recording.program,
            "replies_agree": replies_agree,
            "abstained": abstained,
            "unjudged": unjudged,
            "recording": recording.name,
            "agrees": run.agrees(),
            "excepted": outcome.agrees() && stale.is_empty(),
            "covered": run.covered.len(),
            "recorded": (!reasons.is_empty()).then(|| reasons.iter().copied().collect::<Vec<_>>().join(" ")),
        }));
        if run.agrees() {
            agree = agree.saturating_add(1);
        } else {
            differ = differ.saturating_add(1);
        }
        if let Some(why) = &unjudged {
            ok = false;
            println!("UNJUDGED {label}: {why}");
        }
        match (expected, outcome.agrees(), stale.is_empty()) {
            (None, ..) => {
                ok = false;
                println!(
                    "NEW      {label} ({}; no status recorded)   {}",
                    if run.agrees() { "agrees" } else { "differs" },
                    marks(outcome)
                );
                if !outcome.agrees() {
                    print!("{}", report(recording, FUX_VT, outcome));
                }
            }
            (Some(_), true, true) if run.agrees() => {
                println!("ok       {label}   {}", marks(outcome));
            }
            (Some(_), true, true) => {
                println!(
                    "differs  {label}, as its {} exceptions say: {}",
                    exceptions.len(),
                    reasons.iter().copied().collect::<Vec<_>>().join(" ")
                );
                if !names.is_empty() {
                    for (step, key, e) in &run.covered {
                        println!("  after step {step}: {key} (exception {e})");
                    }
                }
            }
            (Some(_), true, false) => {
                ok = false;
                println!("FAIL     {label}: an exception no longer occurs");
                for e in stale {
                    println!(
                        "  steps {}-{}, {}: {}",
                        e.steps.0, e.steps.1, e.field, e.reason
                    );
                }
            }
            (Some(_), false, _) => {
                ok = false;
                println!(
                    "FAIL     {label} (a difference no exception covers)   {}",
                    marks(outcome)
                );
                print!("{}", report(recording, FUX_VT, outcome));
            }
        }
        if show {
            let end = case.run_until(&[], false)?;
            for line in &end.fux.screen {
                println!("    |{}", line.text());
            }
        }
    }
    if names.is_empty() {
        for (i, (was, is, why)) in REPLY_CHANGES.iter().enumerate() {
            if !shown_changes.contains(&i) {
                ok = false;
                println!(
                    "REPLIES  the change {was:?} to {is:?} ({why}) shows in no recording: remove it"
                );
            }
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    println!("{agree} recordings agree, {differ} differ ({seconds:.1}s)");
    if let Some(path) = json {
        let value = serde_json::json!({
            "check": "corpus",
            "settings": engine::settings(names_of.iter().copied()),
            "engines": names_of,
            "recordings": recordings.len(),
            "agree": agree,
            "differ": differ,
            "seconds": seconds,
            "ok": ok,
            "results": results,
        });
        let mut text =
            serde_json::to_string_pretty(&value).map_err(|e| format!("the results: {e}"))?;
        text.push('\n');
        std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(ok)
}

/// The recordings judged with another engine as the subject, as fux-vt is
/// judged: xterm decides what it can tell, the panel (where fux-vt now
/// votes) the rest. The statuses are fux-vt's, so none is applied: each
/// recording's outcome is reported, and only an error, or a recording
/// xterm could not judge (see `abstentions`), fails.
///
/// Every recording runs to its end, and at each judged point fux-vt is
/// judged too, on the same screens, with the subject voting in its place:
/// both scores, in recordings and in points, and where each fails and
/// the other does not, field by field, come from one run.
fn run_subject(
    subject: usize,
    panel: &[usize],
    recordings: &[Recording],
    named: bool,
    show: bool,
    json: Option<&str>,
) -> Result<bool, String> {
    let name = engine::name(subject);
    let names_of: Vec<&str> = panel.iter().map(|&i| engine::name(i)).collect();
    println!("subject: {name}, judged as fux-vt is (fux-vt's statuses are not applied)");
    println!("engines: {}", names_of.join(" "));
    let started = Instant::now();
    let mut scores = [Score::default(), Score::default()];
    // Whether xterm judged every recording (see `abstentions`).
    let mut whole = true;
    let mut results = Vec::new();
    let (mut fux_ahead, mut subject_ahead) = (Vec::new(), Vec::new());
    for recording in recordings {
        let case = Case {
            subject,
            ..recording.case()
        };
        let (points, outcome) = match judge_both(recording, &case, panel) {
            Ok(both) => both,
            Err(e) => {
                whole = false;
                println!("ERROR    {}: {e}", recording.name);
                results.push(serde_json::json!({ "recording": recording.name, "error": e }));
                continue;
            }
        };
        let mine: Vec<Option<&[(String, Field)]>> =
            points.iter().map(|p| Some(p.subject.as_slice())).collect();
        let fux: Vec<Option<&[(String, Field)]>> =
            points.iter().map(|p| p.fux.as_deref()).collect();
        let judged = [
            Judged::of(&points, &mine, true),
            Judged::of(&points, &fux, false),
        ];
        for (score, j) in scores.iter_mut().zip(&judged) {
            score.add(j, points.len());
        }
        let [me, _] = &judged;
        let label = format!(
            "{} ({} steps, {} bytes; {} of {} points)",
            recording.name,
            recording.steps.len(),
            recording.bytes().len(),
            me.points_agree,
            points.len()
        );
        match &me.first {
            None if me.points_unavailable > 0 => println!(
                "unavail. {label}: {} points on fields {name} cannot tell   {}",
                me.points_unavailable,
                marks(&outcome)
            ),
            None => println!("agrees   {label}   {}", marks(&outcome)),
            Some((step, keys)) => {
                println!(
                    "differs  {label}: {}; on {}   {}",
                    place(recording, Some(*step)),
                    listed(keys),
                    marks(&outcome)
                );
                if named {
                    let first = judge_excepted(recording, &case, panel, &[])?;
                    print!("{}", report(recording, subject, &first.outcome));
                }
            }
        }
        if show {
            let end = case.run_until(&[], false)?;
            for line in &end.fux.screen {
                println!("    |{}", line.text());
            }
        }
        let alone_mine = Alone::of(recording, &points, |p| {
            p.fux.as_ref().map(|f| alone(&p.subject, f))
        });
        let alone_fux = Alone::of(recording, &points, |p| {
            p.fux.as_ref().map(|f| alone(f, &p.subject))
        });
        let [jm, jf] = &judged;
        let (abstained, unjudged) = abstentions(&outcome);
        if let Some(why) = &unjudged {
            whole = false;
            println!("UNJUDGED {}: {why}", recording.name);
        }
        results.push(serde_json::json!({
            "recording": recording.name,
            "program": recording.program,
            "points": points.len(),
            "subject": jm.json(recording),
            "abstained": abstained,
            "unjudged": unjudged,
            "fux-vt": jf.json(recording),
        }));
        fux_ahead.extend(alone_mine);
        subject_ahead.extend(alone_fux);
    }
    let seconds = started.elapsed().as_secs_f64();
    let [mine, fux] = &scores;
    println!(
        "{name}: {} recordings agree, {} differ, {} unavailable; {} of {} points agree, {} \
         unavailable ({seconds:.1}s)",
        mine.agree,
        mine.differ,
        mine.unavailable,
        mine.points_agree,
        mine.points,
        mine.points_unavailable
    );
    println!(
        "fux-vt, judged on the same screens with {name} voting in its place: {} recordings \
         agree, {} differ; {} of {} points agree",
        fux.agree, fux.differ, fux.points_agree, fux.points
    );
    let engines_of = |panel: &[usize]| -> String {
        panel
            .iter()
            .map(|&i| engine::name(i))
            .collect::<Vec<_>>()
            .join(",")
    };
    let swapped: Vec<usize> = panel
        .iter()
        .map(|&i| if i == FUX_VT { subject } else { i })
        .collect();
    let replay_mine = format!(
        "fux-vt-compare corpus --subject {name} --engines {}",
        engines_of(panel)
    );
    let replay_fux = format!("fux-vt-compare corpus --engines {}", engines_of(&swapped));
    println!();
    print_ahead(
        &format!("fux-vt ahead: {name} departs from xterm and the panel where fux-vt does not"),
        &fux_ahead,
        &replay_mine,
    );
    println!();
    print_ahead(
        &format!("{name} ahead: fux-vt departs from xterm and the panel where {name} does not"),
        &subject_ahead,
        &replay_fux,
    );
    if let Some(path) = json {
        let value = serde_json::json!({
            "check": "corpus",
            "settings": engine::settings(names_of.iter().copied()),
            "subject": name,
            "engines": names_of,
            "recordings": recordings.len(),
            "agree": mine.agree,
            "differ": mine.differ,
            "unavailable": mine.unavailable,
            "points": mine.points,
            "points_agree": mine.points_agree,
            "points_unavailable": mine.points_unavailable,
            "fux-vt": {
                "agree": fux.agree,
                "differ": fux.differ,
                "points": fux.points,
                "points_agree": fux.points_agree,
            },
            "seconds": seconds,
            "ok": true,
            "results": results,
            "fux_vt_ahead": ahead_json(&fux_ahead, &replay_mine),
            "subject_ahead": ahead_json(&subject_ahead, &replay_fux),
        });
        let mut text =
            serde_json::to_string_pretty(&value).map_err(|e| format!("the results: {e}"))?;
        text.push('\n');
        std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(whole)
}

/// One judged comparison of a recording: the case's step (as
/// `Outcome::step` counts them) and the fields each fails on there: the
/// subject, and fux-vt judged on the same screens with the subject voting
/// in its place (none if fux-vt abstained).
struct Point {
    step: usize,
    subject: Vec<(String, Field)>,
    /// The fields the subject cannot tell where the judge would have
    /// failed it (`case::reread`): unavailable, so the point is no pass.
    unavailable: Vec<String>,
    fux: Option<Vec<(String, Field)>>,
}

/// Runs a recording's case to its end, judging both the subject and
/// fux-vt at every judged comparison; each engine is fed once. The
/// outcome is the end's.
fn judge_both(
    recording: &Recording,
    case: &Case,
    panel: &[usize],
) -> Result<(Vec<Point>, case::Outcome), String> {
    let judged = recording.judged();
    let at = std::cell::Cell::new(0usize);
    let points = std::cell::RefCell::new(Vec::new());
    let outcome = case.run_judged(panel, false, |base, verdicts| {
        let step = at.get();
        at.set(step.saturating_add(1));
        if !judged.get(step).copied().unwrap_or(true) {
            return false;
        }
        let subject = failing_fields(verdicts);
        let unavailable = failures(&case::reread(base, verdicts, case.subject))
            .into_iter()
            .filter(|key| !subject.iter().any(|(k, _)| k == key))
            .collect();
        let fux = case::rejudge(case.subject, base, verdicts, FUX_VT)
            .map(|(_, theirs)| failing_fields(&theirs));
        let fails = !subject.is_empty();
        points.borrow_mut().push(Point {
            step,
            subject,
            unavailable,
            fux,
        });
        fails
    })?;
    Ok((points.into_inner(), outcome))
}

/// How one subject did on one recording: the points it agrees at, those
/// it could not be judged at (a field it cannot tell, where the judge would
/// have decided: `Point::unavailable`), and where it first fails and on
/// what (none if it never does). Every subject is judged at the same
/// points, on the same fields: one it cannot tell is unavailable there,
/// never a pass.
struct Judged {
    points_agree: usize,
    points_unavailable: usize,
    first: Option<(usize, Vec<String>)>,
}

impl Judged {
    /// From each point's failures for this subject (none: it abstained,
    /// which counts as failing), and, for the subject (`unavailable`),
    /// the fields it could not be judged on.
    fn of(points: &[Point], failed: &[Option<&[(String, Field)]>], unavailable: bool) -> Judged {
        let mut points_agree = 0usize;
        let mut points_unavailable = 0usize;
        let mut first = None;
        for (point, failed) in points.iter().zip(failed) {
            match failed {
                Some([]) if unavailable && !point.unavailable.is_empty() => {
                    points_unavailable = points_unavailable.saturating_add(1);
                }
                Some([]) => points_agree = points_agree.saturating_add(1),
                Some(keys) if first.is_none() => {
                    first = Some((point.step, keys.iter().map(|(k, _)| k.clone()).collect()));
                }
                None if first.is_none() => first = Some((point.step, vec!["abstained".into()])),
                Some(_) | None => {}
            }
        }
        Judged {
            points_agree,
            points_unavailable,
            first,
        }
    }

    /// Whether it agreed at every point, none of them unavailable.
    fn agrees(&self) -> bool {
        self.first.is_none() && self.points_unavailable == 0
    }

    fn json(&self, recording: &Recording) -> serde_json::Value {
        serde_json::json!({
            "agrees": self.agrees(),
            "unavailable": self.first.is_none() && self.points_unavailable > 0,
            "points_agree": self.points_agree,
            "points_unavailable": self.points_unavailable,
            "first": self.first.as_ref().map(|(step, keys)| serde_json::json!({
                "at": place(recording, Some(*step)),
                "fails_on": keys,
            })),
        })
    }
}

/// One subject's totals over the recordings.
#[derive(Default)]
struct Score {
    agree: usize,
    differ: usize,
    /// Recordings it never failed, at a point where it could not be judged.
    unavailable: usize,
    points: usize,
    points_agree: usize,
    points_unavailable: usize,
}

impl Score {
    fn add(&mut self, judged: &Judged, points: usize) {
        if judged.agrees() {
            self.agree = self.agree.saturating_add(1);
        } else if judged.first.is_none() {
            self.unavailable = self.unavailable.saturating_add(1);
        } else {
            self.differ = self.differ.saturating_add(1);
        }
        self.points = self.points.saturating_add(points);
        self.points_agree = self.points_agree.saturating_add(judged.points_agree);
        self.points_unavailable = self
            .points_unavailable
            .saturating_add(judged.points_unavailable);
    }
}

/// The failures in `mine` that are not in `theirs`.
fn alone(mine: &[(String, Field)], theirs: &[(String, Field)]) -> Vec<(String, Field)> {
    mine.iter()
        .filter(|(key, _)| !theirs.iter().any(|(k, _)| k == key))
        .cloned()
        .collect()
}

/// Where one subject fails and the other does not, in one recording: at
/// how many points, the first of them and its fields, and what the fields
/// are about over all of them, with how many points each.
struct Alone {
    recording: String,
    points: usize,
    at: String,
    keys: Vec<String>,
    about: BTreeMap<String, usize>,
}

impl Alone {
    /// From each point's failures of one side alone (none where the other
    /// abstained, which is not counted).
    fn of(
        recording: &Recording,
        points: &[Point],
        alone: impl Fn(&Point) -> Option<Vec<(String, Field)>>,
    ) -> Option<Alone> {
        let mut found: Option<Alone> = None;
        for point in points {
            let Some(fields) = alone(point).filter(|f| !f.is_empty()) else {
                continue;
            };
            let entry = found.get_or_insert_with(|| Alone {
                recording: recording.name.clone(),
                points: 0,
                at: place(recording, Some(point.step)),
                keys: fields.iter().map(|(k, _)| k.clone()).collect(),
                about: BTreeMap::new(),
            });
            entry.points = entry.points.saturating_add(1);
            let kinds: BTreeSet<String> = fields.iter().map(|(_, f)| format!("{f:?}")).collect();
            for kind in kinds {
                let n = entry.about.entry(kind).or_insert(0);
                *n = n.saturating_add(1);
            }
        }
        found
    }
}

/// Fields, by what they are about, with how many points each.
fn about(about: &BTreeMap<String, usize>) -> String {
    about
        .iter()
        .map(|(kind, n)| format!("{kind} {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn print_ahead(title: &str, ahead: &[Alone], replay: &str) {
    let points: usize = ahead.iter().map(|a| a.points).sum();
    let mut total: BTreeMap<String, usize> = BTreeMap::new();
    for a in ahead {
        for (kind, n) in &a.about {
            let t = total.entry(kind.clone()).or_insert(0);
            *t = t.saturating_add(*n);
        }
    }
    println!(
        "{title}: {} recordings, {points} points{}",
        ahead.len(),
        if total.is_empty() {
            String::new()
        } else {
            format!(" (points by field: {})", about(&total))
        }
    );
    for a in ahead {
        println!(
            "  {}: {} points; first {}: {} [{}]",
            a.recording,
            a.points,
            a.at,
            listed(&a.keys),
            about(&a.about)
        );
        println!("    replay: {replay} {}", a.recording);
    }
}

fn ahead_json(ahead: &[Alone], replay: &str) -> serde_json::Value {
    ahead
        .iter()
        .map(|a| {
            serde_json::json!({
                "recording": a.recording,
                "points": a.points,
                "first": a.at,
                "fails_on": a.keys,
                "points_by_field": a.about,
                "replay": format!("{replay} {}", a.recording),
            })
        })
        .collect()
}

/// The first six of `keys`, joined, and `...` if there are more.
fn listed(keys: &[String]) -> String {
    let shown: Vec<&str> = keys.iter().take(6).map(String::as_str).collect();
    let more = if keys.len() > 6 { ", ..." } else { "" };
    format!("{}{more}", shown.join(", "))
}

#[cfg(test)]
mod tests {
    /// A recording's replies are compared with fux-vt's now, versions
    /// aside and with each change since recorded applied: a DA1 reply as
    /// recorded matches the one fux-vt gives since a218350c, a DA2 of
    /// another version matches, and a DA1 reply no change covers does not
    /// (at the parent the replies were never compared).
    #[test]
    fn replies_are_compared_with_the_changes_since_recorded() {
        let (now, then, used) = super::replies_beside(
            b"\x1b[>1;2700;0c\x1b[?62;6;21;22;28c\x1bP>|fux 0.27.0\x1b\\",
            b"\x1b[>1;1700;0c\x1b[?62;22c\x1bP>|fux 0.17.0\x1b\\",
        );
        assert_eq!(now, then);
        assert_eq!(used, [0]);
        let (now, then, _) = super::replies_beside(b"\x1b[?65;1c", b"\x1b[?62;22c");
        assert_ne!(now, then);
        assert_eq!(
            super::unversioned(b"fux 1.2.3 \x1b[>1;99;0c"),
            "fux VERSION \x1b[>1;VERSION;0c"
        );
        assert_eq!(
            super::unversioned(b"\x1b[>1;99;0c fux 1.2.3"),
            "\x1b[>1;VERSION;0c fux VERSION"
        );
    }

    /// Every subject is judged at the same points: one where it fails
    /// nothing but could not be judged on a field is unavailable, neither
    /// a pass nor a failure, and a recording with such a point does not
    /// agree. fux-vt's side, judged on what it tells, is unaffected.
    #[test]
    fn a_point_the_subject_cannot_be_judged_at_is_no_pass() {
        let point = |step, unavailable: &[&str]| super::Point {
            step,
            subject: Vec::new(),
            unavailable: unavailable.iter().map(|u| (*u).to_owned()).collect(),
            fux: Some(Vec::new()),
        };
        let points = [point(1, &[]), point(2, &["cell (0,0) dim"]), point(3, &[])];
        let none: &[(String, crate::snapshot::Field)] = &[];
        let failed = [Some(none), Some(none), Some(none)];
        let subject = super::Judged::of(&points, &failed, true);
        assert_eq!((subject.points_agree, subject.points_unavailable), (2, 1));
        assert!(!subject.agrees() && subject.first.is_none());
        let fux = super::Judged::of(&points, &failed, false);
        assert_eq!((fux.points_agree, fux.points_unavailable), (3, 0));
        assert!(fux.agrees());
    }

    /// xterm, which decides, abstaining for a limit of its adapter's (SGR
    /// 58) leaves the vote to judge, as the module says; abstaining for
    /// anything else (a print that never came) leaves the recording
    /// unjudged. At the parent both left the vote to judge, marked only
    /// `!xterm`. Another engine's abstention unjudges nothing.
    #[test]
    fn a_recording_xterm_could_not_judge_is_unjudged() -> Result<(), String> {
        let xterm = crate::engine::find("xterm").ok_or("no xterm")?;
        let ghostty = crate::engine::find("ghostty").ok_or("no ghostty")?;
        let outcome = |abstained: Vec<(usize, String)>| crate::case::Outcome {
            step: None,
            fux: crate::snapshot::Snapshot::default(),
            verdicts: Vec::new(),
            abstained,
        };
        let limited = format!("{}xterm has no SGR 58", crate::engine::CANNOT);
        let (each, unjudged) = super::abstentions(&outcome(vec![(xterm, limited)]));
        assert_eq!(unjudged, None);
        assert_eq!(
            each.first()
                .and_then(|e| e.get("class"))
                .and_then(|c| c.as_str()),
            Some("adapter-limited")
        );
        let late = "xterm: the print did not arrive".to_owned();
        let (_, unjudged) = super::abstentions(&outcome(vec![(xterm, late.clone())]));
        assert!(unjudged.is_some_and(|u| u.contains("infrastructure")));
        let (_, unjudged) = super::abstentions(&outcome(vec![(ghostty, late)]));
        assert_eq!(unjudged, None);
        assert_eq!(crate::engine::abstention("panicked: at the edge"), "engine");
        Ok(())
    }

    /// zsh-resize's points as its exceptions read them: the reflow's
    /// differences at steps 3, 4 and 8, and its history at 8 and 9, are
    /// covered, and the run goes on past them; a difference on another
    /// field or at another step is covered by none, even after the covered
    /// ones (at the parent, a recording that differs passed whatever
    /// differed after its first difference); an exception that never
    /// occurs is stale; `*` stands for any run of characters.
    #[test]
    fn an_exception_covers_its_steps_and_fields_and_no_other() {
        let exceptions = super::ZSH_RESIZE_AT;
        let mut covered = Vec::new();
        let points = [
            (3, vec!["cell (1,4) text", "cursor", "row 1 soft-wrapped"]),
            (4, vec!["cell (2,0) fg"]),
            (
                8,
                vec![
                    "cell (0,0) text",
                    "history row 12 from the newest",
                    "history rows kept",
                ],
            ),
            (9, vec!["history row 29 from the newest soft-wrapped"]),
        ];
        for (step, keys) in &points {
            let keys = keys.iter().map(|k| (*k).to_owned()).collect();
            assert!(
                !super::sort(*step, keys, exceptions, &mut covered),
                "step {step}"
            );
        }
        assert_eq!(covered.len(), 8);
        let later = |step: usize, key: &str| {
            super::sort(step, vec![key.to_owned()], exceptions, &mut Vec::new())
        };
        assert!(later(9, "cell (3,3) text"));
        assert!(later(5, "cursor"));
        assert!(later(8, "title"));
        let used: std::collections::BTreeSet<usize> = covered.iter().map(|c| c.2).collect();
        let stale: Vec<usize> = (0..exceptions.len())
            .filter(|i| !used.contains(i))
            .collect();
        assert_eq!(stale, [5, 6]);
        assert!(crate::families::glob("cell (*) text", "cell (12,3) text"));
        assert!(!crate::families::glob("cell (*) text", "cell (12,3) width"));
        assert!(crate::families::glob(
            "history row *",
            "history row 3 from the newest"
        ));
        assert!(!crate::families::glob("history row *", "history rows kept"));
        assert!(
            crate::families::glob("title", "title") && !crate::families::glob("title", "titles")
        );
    }

    /// Every recording's bytes are the ones recorded: each `.bin` has its
    /// SHA-256 in `SHA256SUMS`, and it matches.
    #[test]
    fn every_recording_is_its_recorded_bytes() -> Result<(), String> {
        let dir = super::dir();
        let sums = std::fs::read_to_string(dir.join("SHA256SUMS")).map_err(|e| e.to_string())?;
        let mut listed = std::collections::BTreeMap::new();
        for line in sums.lines() {
            let (hash, name) = line
                .split_once("  ")
                .ok_or(format!("SHA256SUMS: {line:?}"))?;
            listed.insert(name.to_owned(), hash.to_owned());
        }
        let mut bins = 0usize;
        for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_none_or(|x| x != "bin") {
                continue;
            }
            bins = bins.saturating_add(1);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            assert_eq!(
                listed.get(&name),
                Some(&crate::bench::sha256(&bytes)?),
                "{name}"
            );
        }
        assert_eq!(bins, listed.len());
        Ok(())
    }

    #[test]
    fn every_recording_loads_with_a_status() -> Result<(), String> {
        let all = super::recordings(&[])?;
        assert!(!all.is_empty());
        for recording in &all {
            assert!(
                super::status(&recording.name).is_some(),
                "{} has no status",
                recording.name
            );
            assert_eq!(recording.steps.first().map(|(k, _)| k.as_str()), Some(""));
            assert!(recording.resize_at(0).is_none());
            assert!(!crate::escape::escape(&recording.bytes()).is_empty());
        }
        Ok(())
    }

    /// A step that resized is two steps of the case: the resize, then the
    /// output after it.
    #[test]
    fn a_step_chunked_is_judged_after_each_piece() {
        let recording = super::Recording {
            name: "r".into(),
            program: "p".into(),
            version: "v".into(),
            rows: 4,
            cols: 10,
            steps: vec![
                (String::new(), b"abcde".to_vec()),
                ("j".into(), b"f".to_vec()),
            ],
            replies: Vec::new(),
            resizes: vec![(1, 3, 8)],
            chunk: Some(2),
        };
        // Before any step; "ab", "cd", "e"; the resize; "f".
        assert_eq!(recording.case().steps.len(), 5);
        assert_eq!(recording.judged(), [true, true, true, true, false, true]);
        let recorded: Vec<Option<usize>> = (0..=5).map(|s| recording.recorded_step(s)).collect();
        assert_eq!(
            recorded,
            [None, Some(0), Some(0), Some(0), Some(1), Some(1)]
        );
    }

    #[test]
    fn a_resize_is_its_own_step_of_the_case() {
        let recording = super::Recording {
            name: "r".into(),
            program: "p".into(),
            version: "v".into(),
            rows: 4,
            cols: 10,
            steps: vec![
                (String::new(), b"a".to_vec()),
                ("j".into(), b"b".to_vec()),
                (String::new(), b"c".to_vec()),
                ("k".into(), b"d".to_vec()),
            ],
            replies: Vec::new(),
            resizes: vec![(2, 3, 8)],
            chunk: None,
        };
        let case = recording.case();
        assert_eq!(case.steps.len(), 5);
        assert_eq!(case.steps.get(2), Some(&crate::case::Step::Resize(3, 8)));
        let recorded: Vec<Option<usize>> = (0..=5).map(|s| recording.recorded_step(s)).collect();
        assert_eq!(
            recorded,
            [None, Some(0), Some(1), Some(2), Some(2), Some(3)]
        );
        // Judged: before any step, after each but the resize itself.
        assert_eq!(recording.judged(), [true, true, true, false, true, true]);
    }
}
