//! The modes a program sets and resets by number. One table names each,
//! its number ([`Mode::of`]) and where it is kept ([`Mode::kind`]), and
//! `Screen::mode` and `Screen::set_mode` read and set every one by its kind.

use crate::{Feature, MouseProtocolEncoding, MouseProtocolMode, Options};

/// Expands `/// doc pub enum Mode { /// doc Variant (private, n) if Feature
/// [default soft] => kind, ... }` (`if Feature` and the marks optional) to
/// that enum, `Mode::of(n, private, options)` (the mode SM and RM, or with
/// `private` DECSET and DECRST, name by `n`; with `if Feature` only while
/// `options` have that feature), `Mode::kind` (each one's kind), `Mode::ALL`
/// (every mode, in the order declared),
/// `Modes::DEFAULT` (the modes marked `default`) and `Modes::SOFT_RESET`
/// (those marked `soft`), and checks at compile time that every mode is a
/// bit of a `Modes` (a `u64`). A row `Autowrap (true, 7) [default soft] =>
/// Kind::Flag,` makes the variant `Autowrap`, the arm `(true, 7) =>
/// Some(Self::Autowrap)` of `of`, the arm `Self::Autowrap => Kind::Flag`
/// of `kind`, `Mode::Autowrap` in `Mode::ALL`, and `| Mode::Autowrap.bit()`
/// in both constants' bits.
macro_rules! modes {
    ($(#[$doc:meta])* pub enum Mode {
        $(
            $(#[$vdoc:meta])*
            $variant:ident ($private:literal, $n:literal) $(if $feature:ident)?
                $([$($mark:ident)*])? => $kind:expr,
        )*
    }) => {
        $(#[$doc])*
        pub enum Mode {
            $($(#[$vdoc])* $variant,)*
        }

        impl Mode {
            /// Every mode, in the order declared.
            pub const ALL: [Self; [$(Self::$variant),*].len()] = [$(Self::$variant),*];

            /// The mode SM and RM (`private` false: IRM and LNM alone) or
            /// DECSET and DECRST name by `n`, with `options`: without
            /// theirs, 2031 and 2048 are not recognized. Out of line, and
            /// read from tables of the numbers, so that its code is the
            /// same whatever numbers there are: a `match` of them, inlined
            /// into `Screen::csi`, slowed every sequence as modes were
            /// added. A DEC private mode is found by its number
            /// (`PRIVATE`), an ANSI mode by a search of the few.
            #[inline(never)]
            pub(crate) fn of(n: u16, private: bool, options: &Options) -> Option<Self> {
                // Each mode's number, 1 << 16 more for a private mode.
                const NUMBERS: [u32; Mode::ALL.len()] = [$(($private as u32) << 16 | $n,)*];
                const FEATURES: [Option<Feature>; Mode::ALL.len()] =
                    [$(modes!(@feature $($feature)?),)*];
                const PRIVATE: [u8; PRIVATE_MODES] = private_modes(&NUMBERS);
                let at = if private {
                    usize::from(*PRIVATE.get(usize::from(n))?).checked_sub(1)?
                } else {
                    NUMBERS.iter().position(|&number| number == u32::from(n))?
                };
                let feature = FEATURES.get(at).copied().flatten();
                let mode = Self::ALL.get(at).copied()?;
                feature.is_none_or(|f| options.has(f)).then_some(mode)
            }

            /// Where its state is kept, and what setting it does.
            #[inline(always)]
            pub(crate) fn kind(self) -> Kind {
                match self {
                    $(Self::$variant => $kind,)*
                }
            }
        }

        impl Modes {
            /// The modes set on a new screen, and after RIS.
            pub(crate) const DEFAULT: Self =
                Self(0 $($($(| modes!(@default $mark Mode::$variant))*)?)*);

            /// The modes DECSTR puts back as they are by default.
            pub(crate) const SOFT_RESET: Self =
                Self(0 $($($(| modes!(@soft $mark Mode::$variant))*)?)*);
        }

        // Each mode is a bit of a `Modes`: at most a `u64`'s 64 (and so
        // has a place `Mode::of`'s table of `u8`s can hold).
        const _: () = assert!([$(Mode::$variant),*].len() <= 64);
    };
    (@feature $feature:ident) => { Some(Feature::$feature) };
    (@feature) => { None };
    (@default default $mode:path) => { $mode.bit() };
    (@default soft $mode:path) => { 0 };
    (@soft soft $mode:path) => { $mode.bit() };
    (@soft default $mode:path) => { 0 };
}

/// The DEC private modes `Mode::of` reads by number: 0 to 2048.
const PRIVATE_MODES: usize = 2049;

/// For each DEC private mode number below `PRIVATE_MODES`, one more than
/// its place in `numbers` (each mode's, `1 << 16` more if private), 0 for
/// none.
const fn private_modes(numbers: &[u32]) -> [u8; PRIVATE_MODES] {
    let mut table = [0u8; PRIVATE_MODES];
    let mut slots: &mut [u8] = &mut table;
    let mut n: u32 = 0;
    while let [slot, rest @ ..] = slots {
        *slot = place(numbers, n);
        slots = rest;
        n = match n.checked_add(1) {
            Some(next) => next,
            None => break,
        };
    }
    table
}

/// One more than the place of DEC private mode `n` in `numbers`, 0 for
/// none.
const fn place(numbers: &[u32], n: u32) -> u8 {
    let key = n | 1 << 16;
    let mut rest = numbers;
    let mut at: u8 = 0;
    while let [number, tail @ ..] = rest {
        at = match at.checked_add(1) {
            Some(next) => next,
            None => return 0,
        };
        if *number == key {
            return at;
        }
        rest = tail;
    }
    0
}

modes! {
    /// A mode a program sets and resets by number: an ANSI mode (SM and RM,
    /// `CSI Ps h` / `l`) or a DEC private mode (DECSET and DECRST, `CSI ? Ps h`
    /// / `l`). [`Screen::mode`](crate::Screen::mode) says whether it is set, as
    /// DECRQM reports it.
    ///
    /// A mode marked `default` is set on a new screen and after RIS. One
    /// marked `soft` DECSTR puts back as it is by default: those of the
    /// VT520 manual's table (p. 5-150) and DEC STD 070's Soft Terminal
    /// Reset (p. 4-37); reverse wraparound, as xterm 411's DECSTR ends
    /// both; and synchronized output, in neither table, so that `tput
    /// init` and `tput reset`, which send DECSTR, never leave a frame
    /// waiting.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub enum Mode {
        /// IRM (`CSI 4 h`): a glyph printed moves what is there right.
        Insert (false, 4) [soft] => Kind::Flag,
        /// LNM (`CSI 20 h`): LF, VT and FF return the carriage too.
        NewLine (false, 20) => Kind::Flag,
        /// DECCKM (`CSI ? 1 h`): cursor keys send their application sequences.
        ApplicationCursor (true, 1) [soft] => Kind::Flag,
        /// DECSCLM, smooth scroll (`CSI ? 4 h`). State only.
        SmoothScroll (true, 4) => Kind::Flag,
        /// DECSCNM, reverse video (`CSI ? 5 h`). State only: the host draws
        /// it.
        ReverseVideo (true, 5) => Kind::Flag,
        /// DECOM (`CSI ? 6 h`): lines are addressed from the top margin.
        Origin (true, 6) => Kind::Origin,
        /// DECAWM (`CSI ? 7 h`): a glyph past the last column wraps. Default.
        Autowrap (true, 7) [default soft] => Kind::Flag,
        /// DECARM (`CSI ? 8 h`): permanently reset, as in xterm 411.
        AutoRepeat (true, 8) => Kind::Permanent(false),
        /// Mouse mode 9, X10: button presses.
        MousePress (true, 9) => Kind::Mouse(MouseProtocolMode::Press),
        /// Mode 12 (`CSI ? 12 h`, ctlseqs: "Start blinking cursor (AT&T
        /// 610)"): the blink of the cursor's shape, which DECSCUSR sets
        /// too, as xterm 411 keeps one state for both.
        CursorBlink (true, 12) => Kind::CursorBlink,
        /// DECTCEM (`CSI ? 25 h`): the cursor is shown. Default.
        ShowCursor (true, 25) [default soft] => Kind::Flag,
        /// more(1)'s fix (`CSI ? 41 h`, ctlseqs): HT with a wrap pending
        /// carries the wrap out first, as xterm's `curses` resource has it.
        MoreFix (true, 41) => Kind::Flag,
        /// Reverse wraparound (`CSI ? 45 h`): BS and CUB go back over soft wraps.
        ReverseWrap (true, 45) [soft] => Kind::Flag,
        /// The alternate screen (`CSI ? 47 h`).
        AlternateScreen (true, 47) => Kind::Screen(Switch::Plain),
        /// DECHCCM (`CSI ? 60 h`), horizontal cursor coupling: permanently
        /// reset, "always Reset in the VT420 and can not be changed" (DEC
        /// STD 070, DECRQM, p. 5-194).
        HorizontalCursorCoupling (true, 60) => Kind::Permanent(false),
        /// DECNKM (`CSI ? 66 h`), the keypad mode `ESC =` and `ESC >` set too.
        ApplicationKeypad (true, 66) [soft] => Kind::Flag,
        /// DECBKM (`CSI ? 67 h`), the backarrow key sending BS, which
        /// `Screen::key_mode` reads.
        BackarrowSendsBackspace (true, 67) => Kind::Flag,
        /// DECLRMM (`CSI ? 69 h`): DECSLRM sets left and right margins.
        LeftRightMargins (true, 69) [soft] => Kind::Margins,
        /// DECSDM (`CSI ? 80 h`), Sixel Display Mode (ctlseqs: "Enable
        /// Sixel Display Mode", "Turns on 'Sixel Scrolling'" when reset):
        /// a picture goes at the top left, scrolls nothing and moves no
        /// cursor, as xterm 411 has it. DEC STD 070 does not define it.
        SixelDisplay (true, 80) => Kind::Flag,
        /// Mouse mode 1000: presses and releases.
        MousePressRelease (true, 1000) => Kind::Mouse(MouseProtocolMode::PressRelease),
        /// Mouse mode 1001, highlight tracking: reported as 1000.
        MouseHighlight (true, 1001) => Kind::Mouse(MouseProtocolMode::Highlight),
        /// Mouse mode 1002: also motion while a button is down.
        MouseButtonMotion (true, 1002) => Kind::Mouse(MouseProtocolMode::ButtonMotion),
        /// Mouse mode 1003: also motion with no button down.
        MouseAnyMotion (true, 1003) => Kind::Mouse(MouseProtocolMode::AnyMotion),
        /// Focus reporting (`CSI ? 1004 h`), as `Screen::focus_report` makes it.
        FocusReporting (true, 1004) => Kind::Flag,
        /// Mouse encoding 1005: coordinates in UTF-8.
        MouseUtf8 (true, 1005) => Kind::Encoding(MouseProtocolEncoding::Utf8),
        /// Mouse encoding 1006, SGR.
        MouseSgr (true, 1006) => Kind::Encoding(MouseProtocolEncoding::Sgr),
        /// Alternate scroll (`CSI ? 1007 h`, xterm's alternateScroll): the
        /// wheel, with no mouse reporting, on the alternate screen sends
        /// cursor up and down (`Screen::encode_mouse`).
        AlternateScroll (true, 1007) => Kind::Flag,
        /// Mouse encoding 1015, urxvt.
        MouseUrxvt (true, 1015) => Kind::Encoding(MouseProtocolEncoding::Urxvt),
        /// Mouse encoding 1016, SGR with pixels.
        MouseSgrPixels (true, 1016) => Kind::Encoding(MouseProtocolEncoding::SgrPixels),
        /// Extended reverse wraparound (`CSI ? 1045 h`): back over any line.
        ExtendedReverseWrap (true, 1045) [soft] => Kind::Flag,
        /// The alternate screen (`CSI ? 1047 h`), cleared on leaving it.
        AlternateScreenCleared (true, 1047) => Kind::Screen(Switch::ClearedOnLeaving),
        /// `CSI ? 1048 h` / `l`: DECSC and DECRC, never set.
        SaveCursor (true, 1048) => Kind::SaveCursor,
        /// The alternate screen (`CSI ? 1049 h`), cleared, the cursor saved.
        AlternateScreenSaveCursor (true, 1049) => Kind::Screen(Switch::SavingCursor),
        /// Bracketed paste (`CSI ? 2004 h`).
        BracketedPaste (true, 2004) => Kind::Flag,
        /// Synchronized output (`CSI ? 2026 h`): a frame the host is to hold.
        SynchronizedOutput (true, 2026) [soft] => Kind::Frame,
        /// Colour-scheme change reports (`CSI ? 2031 h`), which the host sends.
        ColorSchemeUpdates (true, 2031) if ColorSchemeUpdates => Kind::Flag,
        /// In-band resize (`CSI ? 2048 h`): a size report at each resize.
        InBandResize (true, 2048) if InBandResize => Kind::SizeReport,
        /// Grapheme clustering (`CSI ? 2027 h`): permanently set. Graphemes
        /// are always clustered, as kitty's and Ghostty's mode 2027 does, so
        /// a program measuring them counts their cells as fux-vt does.
        GraphemeClustering (true, 2027) => Kind::Permanent(true),
    }
}

/// The ANSI modes ECMA-48 (5th edition) defines that fux-vt keeps in their
/// reset state for good (7.1: "some or all of the modes may have one state
/// only"): GATM, KAM (the keyboard never locked), CRM (controls performed),
/// SRTM, ERM (ED, EL and ECH spare a guarded area), VEM, BDSM (EXPLICIT:
/// no implicit bidirectional processing), DCSM (PRESENTATION: there is no
/// data component), HEM, PUM, FEAM, FETM, MATM, TTM, SATM and TSM, which
/// DECRQM answers permanently reset. And 19, the 4th edition's EBM, which
/// the 5th says "shall not be used" (F.5.1): DECRQM is DEC's, and DEC STD
/// 070's VT420 table (DECRQM, p. 5-194) answers it "always Reset ... and
/// can not be changed".
pub(crate) const RESET_FOR_GOOD: [u16; 17] =
    [1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 13, 14, 15, 16, 17, 18, 19];

/// The ANSI modes fux-vt keeps in their set state for good, which DECRQM
/// answers permanently set: SRM, SIMULTANEOUS (7.2.15; DEC STD 070's SRM,
/// local echo off, its default), as fux-vt never images what is typed;
/// GRCM, CUMULATIVE (7.2.8), as each SGR changes only the renditions it
/// names; ZDM, DEFAULT (F.4.2), as a parameter of 0 is its default.
pub(crate) const SET_FOR_GOOD: [u16; 3] = [12, 21, 22];

impl Mode {
    /// What DECRQM reports for a mode SM and RM never change: 1048, an
    /// action, is not recognized, and DECARM, DECHCCM and 2027 are reset
    /// or set for good. None for every other mode, which XTSAVE saves and XTRESTORE
    /// restores.
    pub(crate) fn fixed(self) -> Option<ModeStatus> {
        match self.kind() {
            Kind::SaveCursor => Some(ModeStatus::NotRecognized),
            Kind::Permanent(true) => Some(ModeStatus::PermanentlySet),
            Kind::Permanent(false) => Some(ModeStatus::PermanentlyReset),
            Kind::Flag
            | Kind::Margins
            | Kind::Frame
            | Kind::SizeReport
            | Kind::Origin
            | Kind::CursorBlink
            | Kind::Screen(_)
            | Kind::Mouse(_)
            | Kind::Encoding(_) => None,
        }
    }

    const fn bit(self) -> u64 {
        1u64.wrapping_shl(self as u32)
    }
}

closed! {
    /// A mode's status, as DECRQM asks for it and DECRPM reports it
    /// (`CSI ? mode ; status $ y`).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub enum ModeStatus {
        /// Its number in DECRPM.
        pub fn number(self) -> u8;
        /// Not a mode the terminal knows (or 1048, an action).
        NotRecognized => 0,
        /// Set.
        Set => 1,
        /// Reset.
        Reset => 2,
        /// Set, and stays so.
        PermanentlySet => 3,
        /// Reset, and stays so.
        PermanentlyReset => 4,
    }
}

/// Where a mode's state is kept.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Kind {
    /// A bit of the screen's [`Modes`], and no more.
    Flag,
    /// DECLRMM: a bit; reset, the margins go back to the screen's edges.
    Margins,
    /// Synchronized output: a bit; set, a frame begins.
    Frame,
    /// In-band resize: a bit; set, a size report is due.
    SizeReport,
    /// The shown grid's DECOM.
    Origin,
    /// Mode 12: whether the cursor's shape blinks.
    CursorBlink,
    /// Whether the alternate screen is shown.
    Screen(Switch),
    /// 1048: an action, never set, and not saved by XTSAVE.
    SaveCursor,
    /// DECARM, permanently reset, and 2027, permanently set: neither
    /// changes, nor is saved by XTSAVE.
    Permanent(bool),
    /// The mouse reporting, the latest set winning.
    Mouse(MouseProtocolMode),
    /// The mouse encoding, the latest set winning.
    Encoding(MouseProtocolEncoding),
}

/// How a mode switches to the alternate screen and back (xterm's ctlseqs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Switch {
    /// 47.
    Plain,
    /// 1047: cleared on leaving it.
    ClearedOnLeaving,
    /// 1049: DECSC, then cleared; and back, then DECRC.
    SavingCursor,
}

/// A set of modes, a bit each.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Modes(u64);

impl Modes {
    pub(crate) const fn contains(self, mode: Mode) -> bool {
        self.0 & mode.bit() != 0
    }

    pub(crate) fn set(&mut self, mode: Mode, on: bool) {
        self.0 = (self.0 & !mode.bit()) | if on { mode.bit() } else { 0 };
    }

    /// The modes in `which` back as they are by default.
    pub(crate) fn reset(&mut self, which: Self) {
        self.0 = (self.0 & !which.0) | (Self::DEFAULT.0 & which.0);
    }
}
