//! The modes a program sets and resets by number. One table names each,
//! its number ([`Mode::of`]) and where it is kept ([`Mode::kind`]), and
//! `Screen::mode` and `Screen::set_mode` read and set every one by its kind.

use crate::{Feature, MouseProtocolEncoding, MouseProtocolMode, Options};

/// Expands `/// doc, Variant (private, n) [if Feature] => kind, ...` to the
/// enum `Mode` of those variants, `Mode::of` (the mode SM and RM, or with
/// `private` DECSET and DECRST, name by `n`; with `if Feature` only while
/// `options` have that feature) and `Mode::kind` (each one's kind).
macro_rules! modes {
    ($(#[$doc:meta])* pub enum Mode {
        $(
            $(#[$vdoc:meta])*
            $variant:ident ($private:literal, $n:literal) $(if $feature:ident)? => $kind:expr,
        )*
    }) => {
        $(#[$doc])*
        pub enum Mode {
            $($(#[$vdoc])* $variant,)*
        }

        impl Mode {
            /// The mode SM and RM (`private` false: IRM and LNM alone) or
            /// DECSET and DECRST name by `n`, with `options`: without
            /// theirs, 2031 and 2048 are not recognized.
            #[inline(always)]
            pub(crate) fn of(n: u16, private: bool, options: &Options) -> Option<Self> {
                match (private, n) {
                    $(
                        ($private, $n) $(if options.has(Feature::$feature))? => Some(Self::$variant),
                    )*
                    _ => None,
                }
            }

            /// Where its state is kept, and what setting it does.
            #[inline(always)]
            pub(crate) fn kind(self) -> Kind {
                match self {
                    $(Self::$variant => $kind,)*
                }
            }
        }
    };
}

modes! {
    /// A mode a program sets and resets by number: an ANSI mode (SM and RM,
    /// `CSI Ps h` / `l`) or a DEC private mode (DECSET and DECRST, `CSI ? Ps h`
    /// / `l`). [`Screen::mode`](crate::Screen::mode) says whether it is set, as
    /// DECRQM reports it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    #[non_exhaustive]
    pub enum Mode {
        /// IRM (`CSI 4 h`): a glyph printed moves what is there right.
        Insert (false, 4) => Kind::Flag,
        /// LNM (`CSI 20 h`): LF, VT and FF return the carriage too.
        NewLine (false, 20) => Kind::Flag,
        /// DECCKM (`CSI ? 1 h`): cursor keys send their application sequences.
        ApplicationCursor (true, 1) => Kind::Flag,
        /// DECSCLM, smooth scroll (`CSI ? 4 h`). State only.
        SmoothScroll (true, 4) => Kind::Flag,
        /// DECSCNM, reverse video (`CSI ? 5 h`). State only.
        ReverseVideo (true, 5) => Kind::Flag,
        /// DECOM (`CSI ? 6 h`): lines are addressed from the top margin.
        Origin (true, 6) => Kind::Origin,
        /// DECAWM (`CSI ? 7 h`): a glyph past the last column wraps. Default.
        Autowrap (true, 7) => Kind::Flag,
        /// DECARM (`CSI ? 8 h`): permanently reset, as in xterm 411.
        AutoRepeat (true, 8) => Kind::Reset,
        /// Mouse mode 9, X10: button presses.
        MousePress (true, 9) => Kind::Mouse(MouseProtocolMode::Press),
        /// DECTCEM (`CSI ? 25 h`): the cursor is shown. Default.
        ShowCursor (true, 25) => Kind::Flag,
        /// Reverse wraparound (`CSI ? 45 h`): BS and CUB go back over soft wraps.
        ReverseWrap (true, 45) => Kind::Flag,
        /// The alternate screen (`CSI ? 47 h`).
        AlternateScreen (true, 47) => Kind::Screen(Switch::Plain),
        /// DECNKM (`CSI ? 66 h`), the keypad mode `ESC =` and `ESC >` set too.
        ApplicationKeypad (true, 66) => Kind::Flag,
        /// DECBKM (`CSI ? 67 h`), the backarrow key sending BS. State only.
        BackarrowSendsBackspace (true, 67) => Kind::Flag,
        /// DECLRMM (`CSI ? 69 h`): DECSLRM sets left and right margins.
        LeftRightMargins (true, 69) => Kind::Margins,
        /// Mouse mode 1000: presses and releases.
        MousePressRelease (true, 1000) => Kind::Mouse(MouseProtocolMode::PressRelease),
        /// Mouse mode 1002: also motion while a button is down.
        MouseButtonMotion (true, 1002) => Kind::Mouse(MouseProtocolMode::ButtonMotion),
        /// Mouse mode 1003: also motion with no button down.
        MouseAnyMotion (true, 1003) => Kind::Mouse(MouseProtocolMode::AnyMotion),
        /// Focus reporting (`CSI ? 1004 h`), as `Screen::encode_focus` makes it.
        FocusReporting (true, 1004) => Kind::Flag,
        /// Mouse encoding 1005: coordinates in UTF-8.
        MouseUtf8 (true, 1005) => Kind::Encoding(MouseProtocolEncoding::Utf8),
        /// Mouse encoding 1006, SGR.
        MouseSgr (true, 1006) => Kind::Encoding(MouseProtocolEncoding::Sgr),
        /// Extended reverse wraparound (`CSI ? 1045 h`): back over any line.
        ExtendedReverseWrap (true, 1045) => Kind::Flag,
        /// The alternate screen (`CSI ? 1047 h`), cleared on leaving it.
        AlternateScreenCleared (true, 1047) => Kind::Screen(Switch::ClearedOnLeaving),
        /// `CSI ? 1048 h` / `l`: DECSC and DECRC, never set.
        SaveCursor (true, 1048) => Kind::SaveCursor,
        /// The alternate screen (`CSI ? 1049 h`), cleared, the cursor saved.
        AlternateScreenSaveCursor (true, 1049) => Kind::Screen(Switch::SavingCursor),
        /// Bracketed paste (`CSI ? 2004 h`).
        BracketedPaste (true, 2004) => Kind::Flag,
        /// Synchronized output (`CSI ? 2026 h`): a frame the host is to hold.
        SynchronizedOutput (true, 2026) => Kind::Frame,
        /// Colour-scheme change reports (`CSI ? 2031 h`), which the host sends.
        ColorSchemeUpdates (true, 2031) if ColorSchemeUpdates => Kind::Flag,
        /// In-band resize (`CSI ? 2048 h`): a size report at each resize.
        InBandResize (true, 2048) if InBandResize => Kind::SizeReport,
    }
}

// Each mode is a bit of a `Modes`.
const _: () = assert!((Mode::InBandResize as u32) < u32::BITS);

impl Mode {
    /// Whether XTSAVE saves it and XTRESTORE restores it: not 1048, an
    /// action, nor DECARM, permanently reset.
    pub(crate) fn savable(self) -> bool {
        !matches!(self.kind(), Kind::SaveCursor | Kind::Reset)
    }

    const fn bit(self) -> u32 {
        1u32.wrapping_shl(self as u32)
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
    /// Whether the alternate screen is shown.
    Screen(Switch),
    /// 1048: an action, never set, and not saved by XTSAVE.
    SaveCursor,
    /// DECARM: permanently reset, and not saved by XTSAVE.
    Reset,
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
pub(crate) struct Modes(u32);

impl Modes {
    /// The modes set on a new screen, and after RIS.
    pub(crate) const DEFAULT: Self = Self(Mode::Autowrap.bit() | Mode::ShowCursor.bit());

    /// The modes DECSTR puts back as they are by default: those of the
    /// VT520 manual's table (p. 5-150) and DEC STD 070's Soft Terminal
    /// Reset (p. 4-37); reverse wraparound, as xterm 411's DECSTR ends
    /// both; and synchronized output, in neither table, so that `tput
    /// init` and `tput reset`, which send DECSTR, never leave a frame
    /// waiting.
    pub(crate) const SOFT_RESET: Self = Self(
        Mode::ShowCursor.bit()
            | Mode::SynchronizedOutput.bit()
            | Mode::Autowrap.bit()
            | Mode::ReverseWrap.bit()
            | Mode::ExtendedReverseWrap.bit()
            | Mode::Insert.bit()
            | Mode::ApplicationCursor.bit()
            | Mode::ApplicationKeypad.bit()
            | Mode::LeftRightMargins.bit(),
    );

    pub(crate) fn contains(self, mode: Mode) -> bool {
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
